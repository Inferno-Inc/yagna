use crate::api::*;
use crate::dao::debit_note::DebitNoteDao;
use crate::dao::invoice::InvoiceDao;
use crate::error::{DbError, Error};
use crate::models as db_models;
use crate::utils::*;
use actix_web::web::{get, post, Data, Json, Path, Query};
use actix_web::{HttpResponse, Scope};
use ya_core_model::ethaddr::NodeId;
use ya_core_model::payment;
use ya_model::payment::*;
use ya_net::RemoteEndpoint;
use ya_persistence::executor::DbExecutor;
use ya_service_api_web::middleware::Identity;
use ya_service_bus::{timeout::IntoTimeoutFuture, RpcEndpoint};

pub fn register_endpoints(scope: Scope) -> Scope {
    scope
        .route("/debitNotes", post().to(issue_debit_note))
        .route("/debitNotes", get().to(get_debit_notes))
        .route("/debitNotes/{debit_note_id}", get().to(get_debit_note))
        .route(
            "/debitNotes/{debit_note_id}/payments",
            get().to(get_debit_note_payments),
        )
        .route(
            "/debitNotes/{debit_note_id}/send",
            post().to(send_debit_note),
        )
        .route(
            "/debitNotes/{debit_note_id}/cancel",
            post().to(cancel_debit_note),
        )
        .route("/debitNoteEvents", get().to(get_debit_note_events))
        .route("/invoices", post().to(issue_invoice))
        .route("/invoices", get().to(get_invoices))
        .route("/invoices/{invoice_id}", get().to(get_invoice))
        .route(
            "/invoices/{invoice_id}/payments",
            get().to(get_invoice_payments),
        )
        .route("/invoices/{invoice_id}/send", post().to(send_invoice))
        .route("/invoices/{invoice_id}/cancel", post().to(cancel_invoice))
        .route("/invoiceEvents", get().to(get_invoice_events))
        .route("/payments", get().to(get_payments))
        .route("/payments/{payment_id}", get().to(get_payment))
}

// ************************** DEBIT NOTE **************************

async fn issue_debit_note(
    db: Data<DbExecutor>,
    body: Json<NewDebitNote>,
    id: Identity,
) -> HttpResponse {
    // TODO: Check if activity exists
    let debit_note = body.into_inner();
    let agreement_id = debit_note.agreement_id.clone();

    let agreement = match get_agreement(agreement_id).await {
        Ok(Some(agreement)) => agreement,
        Ok(None) => {
            return HttpResponse::BadRequest()
                .body(format!("Agreement not found: {}", &debit_note.agreement_id))
        }
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };

    let issuer_id = id.identity.to_string();
    if agreement.offer.provider_id.unwrap() != issuer_id {
        // FIXME: provider_id shouldn't be an Option
        return HttpResponse::Unauthorized().body(format!(
            "Identity {} is not authorized to issue this debit note",
            issuer_id
        ));
    }
    let recipient_id = agreement.demand.requestor_id.unwrap(); // FIXME: requestor_id shouldn't be an Option
    let debit_note = db_models::NewDebitNote::from_api_model(debit_note, issuer_id, recipient_id);
    let debit_note_id = debit_note.id.clone();

    match async move {
        let dao: DebitNoteDao = db.as_dao();
        dao.create(debit_note).await?;
        Ok(dao.get(debit_note_id).await?)
    }
    .await
    {
        Ok(Some(debit_note)) => HttpResponse::Created().json(Into::<DebitNote>::into(debit_note)),
        Ok(None) => HttpResponse::InternalServerError().body("Database error"),
        Err(DbError::Query(e)) => HttpResponse::BadRequest().body(e.to_string()),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}

async fn get_debit_notes(db: Data<DbExecutor>, id: Identity) -> HttpResponse {
    let issuer_id = id.identity.to_string();
    let dao: DebitNoteDao = db.as_dao();
    match dao.get_issued(issuer_id).await {
        Ok(debit_notes) => HttpResponse::Ok().json(
            debit_notes
                .into_iter()
                .map(|d| d.into())
                .collect::<Vec<DebitNote>>(),
        ),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}

async fn get_debit_note(
    db: Data<DbExecutor>,
    path: Path<DebitNoteId>,
    id: Identity,
) -> HttpResponse {
    let issuer_id = id.identity.to_string();
    let dao: DebitNoteDao = db.as_dao();
    match dao.get(path.debit_note_id.clone()).await {
        Ok(Some(debit_note)) if debit_note.issuer_id == issuer_id => {
            HttpResponse::Ok().json(Into::<DebitNote>::into(debit_note))
        }
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
        _ => HttpResponse::NotFound().finish(),
    }
}

async fn send_debit_note(
    db: Data<DbExecutor>,
    path: Path<DebitNoteId>,
    query: Query<Timeout>,
    id: Identity,
) -> HttpResponse {
    let dao: DebitNoteDao = db.as_dao();
    let debit_note: DebitNote = match dao.get(path.debit_note_id.clone()).await {
        Ok(Some(debit_note)) => debit_note.into(),
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    // TODO: Check status
    let debit_note_id = debit_note.debit_note_id.clone();

    let node_id = id.identity;
    if Some(node_id) != debit_note.issuer_id.parse().ok() {
        return HttpResponse::Unauthorized().body(format!(
            "Identity {:?} is not authorized to send this debit note",
            node_id,
        ));
    }

    with_timeout(query.timeout, async move {
        let recipient_id: NodeId = debit_note.recipient_id.parse().unwrap();
        let result = match recipient_id
            .service(payment::BUS_ID)
            .call(payment::SendDebitNote(debit_note))
            .await
        {
            Ok(v) => v,
            Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
        };

        match result {
            Ok(_) => (),
            Err(payment::SendError::BadRequest(msg)) => {
                return HttpResponse::BadRequest().body(msg)
            }
            Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
        }
        match dao
            .update_status(debit_note_id, InvoiceStatus::Received.into())
            .await
        {
            Ok(_) => HttpResponse::Ok().finish(),
            Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
        }
    })
    .await
}

async fn cancel_debit_note(
    db: Data<DbExecutor>,
    path: Path<DebitNoteId>,
    query: Query<Timeout>,
) -> HttpResponse {
    HttpResponse::NotImplemented().finish() // TODO
}

async fn get_debit_note_events(db: Data<DbExecutor>, query: Query<EventParams>) -> HttpResponse {
    HttpResponse::NotImplemented().finish() // TODO
}

// *************************** INVOICE ****************************

async fn issue_invoice(db: Data<DbExecutor>, body: Json<NewInvoice>, id: Identity) -> HttpResponse {
    // TODO: Check if activities exists
    let invoice = body.into_inner();
    let agreement_id = invoice.agreement_id.clone();

    let agreement = match get_agreement(agreement_id).await {
        Ok(Some(agreement)) => agreement,
        Ok(None) => {
            return HttpResponse::BadRequest()
                .body(format!("Agreement not found: {}", &invoice.agreement_id))
        }
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };

    let issuer_id = id.identity.to_string();
    if agreement.offer.provider_id.unwrap() != issuer_id {
        // FIXME: provider_id shouldn't be an Option
        return HttpResponse::Unauthorized().body(format!(
            "Identity {} is not authorized to issue this invoice",
            issuer_id
        ));
    }
    let recipient_id = agreement.demand.requestor_id.unwrap(); // FIXME: requestor_id shouldn't be an Option
    let invoice = db_models::NewInvoice::from_api_model(invoice, issuer_id, recipient_id);
    let invoice_id = invoice.invoice.id.clone();

    match async move {
        let dao: InvoiceDao = db.as_dao();
        dao.create(invoice).await?;
        Ok(dao.get(invoice_id).await?)
    }
    .await
    {
        Ok(Some(invoice)) => HttpResponse::Created().json(Into::<Invoice>::into(invoice)),
        Ok(None) => HttpResponse::InternalServerError().body("Database error"),
        Err(DbError::Query(e)) => HttpResponse::BadRequest().body(e.to_string()),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}

async fn get_invoices(db: Data<DbExecutor>, id: Identity) -> HttpResponse {
    let issuer_id = id.identity.to_string();
    let dao: InvoiceDao = db.as_dao();
    match dao.get_issued(issuer_id).await {
        Ok(invoices) => HttpResponse::Ok().json(
            invoices
                .into_iter()
                .map(|d| d.into())
                .collect::<Vec<Invoice>>(),
        ),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}

async fn get_invoice(db: Data<DbExecutor>, path: Path<InvoiceId>, id: Identity) -> HttpResponse {
    let issuer_id = id.identity.to_string();
    let dao: InvoiceDao = db.as_dao();
    match dao.get(path.invoice_id.clone()).await {
        Ok(Some(invoice)) if invoice.invoice.issuer_id == issuer_id => {
            HttpResponse::Ok().json(Into::<Invoice>::into(invoice))
        }
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
        _ => HttpResponse::NotFound().finish(),
    }
}

async fn send_invoice(
    db: Data<DbExecutor>,
    path: Path<InvoiceId>,
    query: Query<Timeout>,
    id: Identity,
) -> HttpResponse {
    let dao: InvoiceDao = db.as_dao();
    let invoice: Invoice = match dao.get(path.invoice_id.clone()).await {
        Ok(Some(invoice)) => invoice.into(),
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    let invoice_id = invoice.invoice_id.clone();

    let node_id = id.identity;
    if Some(node_id) != invoice.issuer_id.parse().ok() {
        return HttpResponse::Unauthorized().body(format!(
            "Identity {:?} is not authorized to send this debit note",
            node_id,
        ));
    }

    let addr: NodeId = invoice.recipient_id.parse().unwrap();
    let msg = payment::SendInvoice(invoice);
    let timeout = if query.timeout > 0 {
        Some(query.timeout * 1000)
    } else {
        None
    };
    match async move {
        addr.service(payment::BUS_ID)
            .send(msg)
            .timeout(timeout)
            .await???;
        Ok(())
    }
    .await
    {
        Err(Error::Timeout(_)) => return HttpResponse::GatewayTimeout().finish(),
        Err(Error::Rpc(payment::RpcMessageError::Send(payment::SendError::BadRequest(e)))) => {
            return { HttpResponse::BadRequest().body(e) }
        }
        Err(e) => return { HttpResponse::InternalServerError().body(e.to_string()) },
        _ => {}
    }

    match dao
        .update_status(invoice_id, InvoiceStatus::Received.into())
        .await
    {
        Ok(_) => HttpResponse::Ok().finish(),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}

async fn cancel_invoice(
    db: Data<DbExecutor>,
    path: Path<InvoiceId>,
    query: Query<Timeout>,
) -> HttpResponse {
    HttpResponse::NotImplemented().finish() // TODO
}

async fn get_invoice_events(db: Data<DbExecutor>, query: Query<EventParams>) -> HttpResponse {
    HttpResponse::NotImplemented().finish() // TODO
}

// *************************** PAYMENT ****************************

async fn get_payments(db: Data<DbExecutor>, query: Query<EventParams>) -> HttpResponse {
    HttpResponse::NotImplemented().finish() // TODO
}

async fn get_payment(db: Data<DbExecutor>, path: Path<PaymentId>) -> HttpResponse {
    HttpResponse::NotImplemented().finish() // TODO
}

async fn get_debit_note_payments(db: Data<DbExecutor>, path: Path<DebitNoteId>) -> HttpResponse {
    HttpResponse::NotImplemented().finish() // TODO
}

async fn get_invoice_payments(db: Data<DbExecutor>, path: Path<InvoiceId>) -> HttpResponse {
    HttpResponse::NotImplemented().finish() // TODO
}