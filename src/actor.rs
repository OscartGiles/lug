use std::{
    fmt::Display,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Future, FutureExt, Stream};
use thiserror::Error;
use tokio::sync::mpsc::{self};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tokio_util::sync::CancellationToken;

pub(crate) trait Actor
where
    Self: 'static + Sized + Send,
{
    /// The type of message the actor can receive.
    type Message: Send;

    /// The type of state the actor holds.
    type State: Send;

    /// Runs when the actor is started.
    fn on_start(&self, _state: &mut Self::State) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }

    /// Runs when the actor attempts gracefully shutdown.
    fn on_stop(self, _state: &mut Self::State) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }

    /// The [Actor]'s event loop.
    /// The default implementation simple iterates over a stream of messages and calls [`Actor::handle`] for each message.
    /// Override this method if the actor needs to handle events other than messages.
    fn run(
        self,
        mut message_stream: impl Stream<Item = Self::Message> + Send + 'static + std::marker::Unpin,
        mut state: Self::State,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            loop {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        self.on_stop(&mut state).await;
                        break;
                    },
                    message = message_stream.next() => {
                        match message {
                            Some(message) => self.handle(message, &mut state).await,
                            None => {
                                self.on_stop(&mut state).await;
                                break
                            },
                        };
                    }
                }
            }
        }
    }

    /// Handle a message sent to the actor.
    fn handle(
        &self,
        message: Self::Message,
        state: &mut Self::State,
    ) -> impl std::future::Future<Output = ()> + Send;

    /// Spawn an [Actor] with an initial state.
    /// The [Actor] will be moved into a tokio task and started.
    /// Returns a tuple containing an [Addr] to send messages to the actor and an [ActorHandle] to gracefully shutdown the actor.
    fn spawn<K>(actor: Self, state: Self::State) -> (K, ActorHandle)
    where
        K: From<Addr<Self>>,
    {
        let (sender, receiver) = mpsc::channel::<Self::Message>(1000);

        let cancellation_token = CancellationToken::new();
        let actors_cancel_token = cancellation_token.clone();

        let handle = tokio::spawn(async move {
            let mut state = state;
            actor.on_start(&mut state).await;
            actor
                .run(ReceiverStream::new(receiver), state, actors_cancel_token)
                .await;
        });

        let addr = Addr::<Self>::new(sender);

        (
            addr.into(),
            ActorHandle {
                task_handle: handle,
                cancellation_token,
            },
        )
    }
}

/// A handle to an actor that can be used to gracefully shutdown the actor or abort it.
pub struct ActorHandle {
    task_handle: tokio::task::JoinHandle<()>,
    cancellation_token: CancellationToken,
}

impl ActorHandle {
    /// Abort the actor.
    /// Internally this calls [`tokio::task::JoinHandle::abort`].
    pub fn abort(&self) {
        self.task_handle.abort();
    }

    /// Gracefully shutdown the actor.
    /// This cancel's the actor's cancellation token which should call [`Actor::on_stop`] and wait for the actor to finish processing messages.
    pub async fn graceful_shutdown(self) -> Result<(), tokio::task::JoinError> {
        self.cancellation_token.cancel();
        self.task_handle.await
    }
}

impl Future for ActorHandle {
    type Output = Result<(), tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.task_handle.poll_unpin(cx)
    }
}

/// An address to send messages to an actor.
pub(crate) struct Addr<A: Actor> {
    sender: mpsc::Sender<A::Message>,
}

impl<A: Actor> Clone for Addr<A> {
    fn clone(&self) -> Self {
        Addr {
            sender: self.sender.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub struct AddrError<M>(#[source] mpsc::error::SendError<M>);

impl<M> Display for AddrError<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Failed to send message to actor")
    }
}

impl<A: Actor> Addr<A> {
    fn new(sender: mpsc::Sender<A::Message>) -> Self {
        Addr { sender }
    }

    pub async fn send(&self, message: A::Message) -> Result<(), AddrError<A::Message>> {
        self.sender.send(message).await.map_err(AddrError)
    }
}
