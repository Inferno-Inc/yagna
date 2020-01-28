use actix::prelude::dev::ToEnvelope;
use actix::prelude::*;
use log::error;

/// Generates actix handler function, that forwards function call
/// to class member function ($ForwardFun). $ForwardFun should be async
/// function. $ActorType should be structure that wraps implementation
/// inside Rc<RefCell<>>. $ActorImpl is field inside $ActorType that should be used
/// as self for $ForwardFun.
#[macro_export]
macro_rules! gen_actix_handler_async {
    ($ActorType:ty, $MessageType:ty, $ForwardFun:tt, $ActorImpl:tt) => {
        impl Handler<$MessageType> for $ActorType {
            type Result = ActorResponse<Self, (), Error>;

            fn handle(&mut self, msg: $MessageType, _ctx: &mut Context<Self>) -> Self::Result {
                let actor_impl = self.$ActorImpl.clone();
                ActorResponse::r#async(
                    async move { (*actor_impl).borrow_mut().$ForwardFun(msg).await }
                        .into_actor(self),
                )
            }
        }
    };
} // gen_actix_handler_async

/// Check docs for gen_actix_handler_async.
#[macro_export]
macro_rules! gen_actix_handler_sync {
    ($ActorType:ty, $MessageType:ty, $ForwardFun:tt, $ActorImpl:tt) => {
        impl Handler<$MessageType> for $ActorType {
            type Result = ActorResponse<Self, (), Error>;

            fn handle(&mut self, msg: $MessageType, _ctx: &mut Context<Self>) -> Self::Result {
                ActorResponse::reply((*self.$ActorImpl).borrow_mut().$ForwardFun(msg))
            }
        }
    };
} // gen_actix_handler_sync

// Sends message to other actor.
pub fn send_message<ActorType, MessageType>(actor: Addr<ActorType>, msg: MessageType)
where
    MessageType: Message + Send + 'static,
    MessageType::Result: Send,
    ActorType: Handler<MessageType>,
    ActorType::Context: ToEnvelope<ActorType, MessageType>,
{
    let future = async move {
        if let Err(error) = actor.send(msg).await {
            //TODO: We could print more information about error.
            error!("Error sending message: {}.", error);
        };
    };
    Arbiter::spawn(future);
}