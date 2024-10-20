use std::{
    any::{Any, TypeId},
    collections::HashMap,
    marker::PhantomData,
    sync::Arc,
};

use crate::actor::{Actor, ActorHandle, Addr, AddrError};
use futures::{Stream, StreamExt};
use std::fmt::Debug;
use tokio::sync::{
    mpsc::{self, Receiver},
    oneshot,
};
use tokio_util::sync::CancellationToken;

type ConsumerId = u64;

/// A [Topic] is a type which defines a topic that [Producer]s can send messages to and [Consumer]s can receive messages from.
pub trait Topic: 'static + Send + Sync {
    /// The type of message that can be sent and received for the topic.
    type MessageType: Send + Sync;
}

#[derive(Clone, Debug)]
pub struct Event {
    payload: Arc<dyn Any + Send + Sync>,
    topic: Arc<dyn Any + Send + Sync>,
}

pub enum EventMessage {
    Event(Event),
    Subscribe {
        topic_id: TypeId,
        sender: oneshot::Sender<ConsumerReceiver>,
        filter: OptionalBoxedFilter,
    },
    Stats(oneshot::Sender<EventBusStats>),
}

impl Debug for EventMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Event(arg0) => f.debug_tuple("Event").field(arg0).finish(),
            Self::Subscribe {
                topic_id,
                sender,
                filter: _,
            } => f
                .debug_struct("Subscribe")
                .field("topic_id", topic_id)
                .field("sender", sender)
                .finish(),
            EventMessage::Stats(_) => f.debug_struct("Request stats").finish(),
        }
    }
}

type OptionalBoxedFilter = Option<Box<dyn Fn(&Arc<dyn Any + Send + Sync>) -> bool + Send + Sync>>;

struct EventSender {
    sender: mpsc::Sender<Event>,
    filter: OptionalBoxedFilter,
}

impl EventSender {
    fn new(sender: mpsc::Sender<Event>, filter: OptionalBoxedFilter) -> Self {
        Self { sender, filter }
    }
}

type DropChannel = (
    mpsc::UnboundedSender<(TypeId, ConsumerId)>,
    mpsc::UnboundedReceiver<(TypeId, ConsumerId)>,
);

pub(crate) struct EventSinkState {
    listeners: HashMap<TypeId, HashMap<ConsumerId, EventSender>>,
    consumer_id_seed: ConsumerId,
    drop_consumer_channel: DropChannel,
}

impl EventSinkState {
    pub(crate) fn new() -> Self {
        Self {
            listeners: HashMap::new(),
            consumer_id_seed: 0,
            drop_consumer_channel: mpsc::unbounded_channel(),
        }
    }

    fn get_topic_listeners(
        &self,
        topic_any: Arc<dyn Any + Send + Sync>,
    ) -> Option<impl Iterator<Item = &EventSender>> {
        // Get the topic id of dyn TopicAny, not Arc<dyn TopicAny>. So dereference before calling.
        let id = (*topic_any).type_id();

        self.listeners.get(&id).map(|topic_listeners| {
            topic_listeners
                .iter()
                .filter(move |(_, s)| {
                    if let Some(filter) = &s.filter {
                        (filter)(&topic_any)
                    } else {
                        true
                    }
                })
                .map(|(_, s)| s)
        })
    }

    /// Remove a consumer from the EventBus. No more messages will be sent to the Consumer.
    fn remove_consumer(&mut self, topic_id: TypeId, consumer_id: ConsumerId) {
        if let Some(topic_listeners) = self.listeners.get_mut(&topic_id) {
            topic_listeners.remove(&consumer_id);

            // Remove the topic if there are no more listeners.
            if topic_listeners.is_empty() {
                self.listeners.remove(&topic_id);
            }
        }
    }
}
impl Default for EventSinkState {
    fn default() -> Self {
        Self::new()
    }
}

/// An EventBus Actor.
///
/// The EventBus is a pub/sub system where actors can subscribe to a topic and receive messages sent to that topic.
///
/// The EventBus allows registering [Producer]s and [Consumer]s for [Topic]s.
/// A [Topic] can have multiple [Consumer]s and multiple [Producer]s.
pub struct EventBus {
    channel_capacity: usize,
}

impl EventBus {
    fn new(channel_capacity: usize) -> Self {
        Self { channel_capacity }
    }

    /// Create a new EventBus with a channel capacity.
    /// The channel capacity is the maximum number of messages that can be buffered in the channel.
    /// Each topic has its own channel.
    /// When a [Producer] sends a message to a topic, the message is sent to the channel for that topic.
    /// If a [Consumer] that is subscribed to that topic does not receive the message in time, the message will be dropped.
    pub fn spawn(channel_capacity: usize) -> (EventBusAddr, ActorHandle) {
        let (addr, handle) = Actor::spawn(EventBus::new(channel_capacity), EventSinkState::new());

        (EventBusAddr(addr), handle)
    }
}

impl Actor for EventBus {
    type Message = EventMessage;
    type State = EventSinkState;

    async fn run(
        self,
        mut message_stream: impl Stream<Item = Self::Message> + Send + 'static + std::marker::Unpin,
        mut state: Self::State,
        cancellation_token: CancellationToken,
    ) {
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
                },
                Some((topic_id, consumer_id)) = state.drop_consumer_channel.1.recv() => {

                    println!("Dropping consumer {:?}", consumer_id);
                    state.remove_consumer(topic_id, consumer_id);
                }
            }
        }
    }

    async fn handle(&self, message: Self::Message, state: &mut Self::State) {
        match message {
            EventMessage::Event(event) => {
                let listeners = state.get_topic_listeners(event.topic.clone());

                if let Some(listeners) = listeners {
                    for l in listeners.into_iter() {
                        l.sender.send(event.clone()).await.unwrap();
                    }
                }
            }
            EventMessage::Subscribe {
                topic_id,
                sender,
                filter,
            } => {
                let topic_listerners = state.listeners.entry(topic_id).or_default();
                let (send, receiver) = mpsc::channel(self.channel_capacity);
                topic_listerners.insert(state.consumer_id_seed, EventSender::new(send, filter));

                if let Err(e) = sender.send(ConsumerReceiver {
                    receiver,
                    consumer_id: state.consumer_id_seed,
                    drop_sender: state.drop_consumer_channel.0.clone(),
                    topic_id,
                }) {
                    println!("Failed to send receiver: {:?}", e);
                }

                state.consumer_id_seed += 1; // Allows 2^64 - 1 consumers before overflow
            }
            EventMessage::Stats(sender) => {
                let n_topics = state.listeners.len();
                sender.send(EventBusStats { n_topics }).unwrap();
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecoveredEvent<T: Topic> {
    payload: Arc<dyn Any + Send + Sync>,
    phantom: PhantomData<T>,
}

impl<T: Topic> AsRef<T::MessageType> for RecoveredEvent<T> {
    fn as_ref(&self) -> &T::MessageType {
        match self.payload.downcast_ref::<T::MessageType>() {
            Some(value) => value,
            None => panic!(
                "Could not downcast. This should never happen and is a bug. Please report it."
            ),
        }
    }
}

/// A wrapper to hold a [Consumer<T>]s event Receiver channel.
/// When this type is dropped it sends the consumer's consumer_id to the EventBus so it can remove it from its listeners.
#[derive(Debug)]
pub struct ConsumerReceiver {
    receiver: Receiver<Event>,
    consumer_id: ConsumerId,
    drop_sender: mpsc::UnboundedSender<(TypeId, ConsumerId)>,
    topic_id: TypeId,
}

/// A Consumer for a topic.
pub struct Consumer<T: Topic> {
    receiver: ConsumerReceiver,
    phantom: PhantomData<T>,
}

impl Drop for ConsumerReceiver {
    fn drop(&mut self) {
        if self
            .drop_sender
            .send((self.topic_id, self.consumer_id))
            .is_err()
        {
            // This could fail if the EventBus has already been dropped - which requires no further action required.
        }
    }
}

impl<T: Topic> Consumer<T> {
    /// Receive a message from the topic.
    pub async fn recv(&mut self) -> Option<RecoveredEvent<T>> {
        self.receiver.receiver.recv().await.map(|e| RecoveredEvent {
            payload: e.payload,
            phantom: PhantomData,
        })
    }

    /// Receive a message from the topic.
    pub fn try_recv(
        &mut self,
    ) -> Result<RecoveredEvent<T>, tokio::sync::mpsc::error::TryRecvError> {
        self.receiver.receiver.try_recv().map(|e| RecoveredEvent {
            payload: e.payload,
            phantom: PhantomData,
        })
    }

    /// Convert the consumer to a [Stream].
    pub fn to_stream(self) -> impl Stream<Item = RecoveredEvent<T>> {
        // ReceiverStream::new(self.receiver.receiver).map(|elem| RecoveredEvent {
        //     payload: elem.payload,
        //     phantom: PhantomData,
        // })

        ConsumerStream::new(self.receiver).map(|elem| RecoveredEvent {
            payload: elem.payload,
            phantom: PhantomData,
        })
    }
}

/// Implement [future::Stream] for [ConsumerReceiver].
/// This type is used in place of [tokio_stream::ReceiverStream]. It holds a [ConsumerReceiver]
/// which sends a message to the EventBus when dropped.
struct ConsumerStream(ConsumerReceiver);

impl ConsumerStream {
    fn new(receiver: ConsumerReceiver) -> Self {
        Self(receiver)
    }
}

impl futures::Stream for ConsumerStream {
    type Item = Event;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.receiver.poll_recv(cx)
    }
}

/// An EventBusAddr is a handle to an EventBus.
/// Use to create [Producer]s and [Consumer]s for topics.
pub struct EventBusAddr(Addr<EventBus>);

impl From<Addr<EventBus>> for EventBusAddr {
    fn from(addr: Addr<EventBus>) -> Self {
        Self(addr)
    }
}

impl EventBusAddr {
    /// Create a new consumer for a topic.
    pub async fn consumer<T: Topic>(&self) -> Result<Consumer<T>, AddrError<EventMessage>> {
        self.consumer_with_filter(Filter::none).await
    }

    /// Create a new consumer for a topic passing a filter.
    pub async fn consumer_with_filter<T: Topic, F>(
        &self,
        f: F,
    ) -> Result<Consumer<T>, AddrError<EventMessage>>
    where
        F: Fn(&T) -> bool + Send + Sync + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let topic_id = TypeId::of::<T>();

        let f = move |item: &Arc<dyn Any + Send + Sync>| {
            // Downcast
            let value = match item.downcast_ref::<T>() {
                Some(value) => value,
                None => panic!(
                    "Could not downcast. This should never happen and is a bug. Please report it."
                ),
            };

            f(value)
        };

        self.0
            .send(EventMessage::Subscribe {
                topic_id,
                sender: tx,
                filter: Some(Box::new(f)),
            })
            .await?;

        let receiver = rx.await.unwrap();
        Ok(Consumer {
            receiver,
            phantom: PhantomData,
        })
    }

    /// Create a new producer for a topic.
    pub fn producer<T>(&self, topic: T) -> Producer<T>
    where
        T: Topic,
    {
        Producer {
            addr: self.0.clone(),
            topic: Arc::new(topic),
        }
    }

    pub async fn stats(&self) -> Result<EventBusStats, AddrError<EventMessage>> {
        let (tx, rx) = oneshot::channel();

        self.0.send(EventMessage::Stats(tx)).await?;

        Ok(rx.await.unwrap())
    }
}

#[derive(Debug)]
pub struct EventBusStats {
    /// Number of registered topics. Decrements when all listeners for a topic are dropped.
    pub n_topics: usize,
}

/// A Producer for a topic.
pub struct Producer<T: Topic> {
    addr: Addr<EventBus>,
    topic: Arc<T>,
}

impl<T: Topic> Producer<T> {
    /// Send a message to the topic.
    pub async fn send(&self, message: T::MessageType) -> Result<(), AddrError<EventMessage>> {
        let t = self.topic.clone();

        let message = Event {
            payload: Arc::new(message),
            topic: t,
        };
        self.addr.send(EventMessage::Event(message)).await?;

        Ok(())
    }
}

pub struct Filter;

impl Filter {
    pub fn none<T: Topic>(_: &T) -> bool {
        true
    }
}
