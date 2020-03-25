use crate::error::Error;
use crate::{
    abortable_sink, abortable_stream, TransferData, TransferProvider, TransferSink, TransferStream,
};
use actix_rt::System;
use bytes::Bytes;
use futures::future::ready;
use futures::{SinkExt, StreamExt, TryFutureExt, TryStreamExt};
use gftp::DEFAULT_CHUNK_SIZE;
use std::cmp::min;
use std::thread;
use url::Url;
use ya_core_model::gftp as model;
use ya_core_model::gftp::Error as GftpError;
use ya_core_model::gftp::GftpChunk;
use ya_net::RemoteEndpoint;
use ya_service_bus::RpcEndpoint;

pub struct GftpTransferProvider {
    rx_buffer_sz: usize,
}

impl Default for GftpTransferProvider {
    fn default() -> Self {
        GftpTransferProvider { rx_buffer_sz: 12 }
    }
}

impl TransferProvider<TransferData, Error> for GftpTransferProvider {
    fn schemes(&self) -> Vec<&'static str> {
        vec!["gftp"]
    }

    fn source(&self, url: &Url) -> TransferStream<TransferData, Error> {
        let url = url.clone();
        let buffer_sz = self.rx_buffer_sz;
        let chunk_size = DEFAULT_CHUNK_SIZE;

        let (stream, tx, abort_reg) = TransferStream::<TransferData, Error>::create(1);
        let txc = tx.clone();

        thread::spawn(move || {
            let fut = async move {
                let (node_id, hash) = gftp::extract_url(&url)
                    .map_err(|_| Error::InvalidUrlError("Invalid gftp URL".to_owned()))?;

                let remote = node_id.service(&model::file_bus_id(&hash));
                let meta = remote.send(model::GetMetadata {}).await??;
                let n = (meta.file_size + chunk_size - 1) / chunk_size;

                futures::stream::iter(0..n)
                    .map(|chunk_number| {
                        remote.call(model::GetChunk {
                            offset: chunk_number * chunk_size,
                            size: chunk_size,
                        })
                    })
                    .buffered(buffer_sz)
                    .map_err(Error::from)
                    .forward(tx.sink_map_err(Error::from).with(
                        |r: Result<GftpChunk, GftpError>| {
                            ready(Ok(match r {
                                Ok(c) => Ok(TransferData::from(Into::<Bytes>::into(c.content))),
                                Err(e) => Err(Error::from(e)),
                            }))
                        },
                    ))
                    .await
                    .map_err(Error::from)
            };

            System::new("tx-gftp").block_on(abortable_stream(fut, abort_reg, txc))
        });

        stream
    }

    fn destination(&self, url: &Url) -> TransferSink<TransferData, Error> {
        let url = url.clone();
        let chunk_size = DEFAULT_CHUNK_SIZE as usize;

        let (sink, mut rx, res_tx) = TransferSink::<TransferData, Error>::create(1);

        thread::spawn(move || {
            let fut = async move {
                let (node_id, random_filename) = gftp::extract_url(&url)
                    .map_err(|_| Error::InvalidUrlError("Invalid gftp URL".to_owned()))?;
                let remote = node_id.service(&model::file_bus_id(&random_filename));

                let mut offset: usize = 0;

                while let Some(result) = rx.next().await {
                    let bytes = result?.into_bytes();
                    let n = (bytes.len() + chunk_size - 1) / chunk_size;

                    for i in 0..n {
                        let start = i * chunk_size;
                        let end = start + min(bytes.len() - start, chunk_size);
                        let content = bytes[start..end].to_vec();

                        let chunk = GftpChunk {
                            offset: offset as u64,
                            content,
                        };
                        offset += chunk.content.len();

                        remote.call(model::UploadChunk { chunk }).await??;
                    }
                }

                Result::<(), Error>::Ok(())
            }
            .map_err(Error::from);

            System::new("rx-gftp").block_on(abortable_sink(fut, res_tx))
        });

        sink
    }
}