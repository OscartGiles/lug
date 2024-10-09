//! lug is an in-process type safe event-bus bus that uses Rust types to define topics and subtopics.
//!
//! ## Topics
//! The [Topic] trait can be implemented to define a topic.
//!
//! ```rust
//! # use tokio_test;
//! use lug::{EventBus, Topic, Filter};
//!
//! struct HelloTopic;
//!
//! impl Topic for HelloTopic {
//!     type MessageType = String;
//! }
//!
//! # tokio_test::block_on(async {
//! // Create an EventBus
//! let (addr, handle) = EventBus::spawn(100);
//!
//! // Subscribe to the topic
//! let mut consumer = addr
//!     .consumer::<HelloTopic>()
//!     .await
//!     .unwrap();
//!
//! // Create a producer for the topic and send a message
//! let producer = addr.producer(HelloTopic);
//! producer
//!     .send("Hello from topic a".to_string())
//!     .await
//!     .unwrap();
//!
//! // Receive a message from the topic
//! let res = consumer.recv().await.unwrap();
//! let value = res.as_ref();
//!
//! assert_eq!(value, &"Hello from topic a".to_string());
//! // Stop the actor
//! handle.graceful_shutdown().await.unwrap();
//!
//! # })
//! ```
//!
//! ## Subtopics
//!
//! Structs that implement [Topic] can have fields. The value of these fields define "subtopics".
//!
//! Subtopics allow for more fine grained control over which messages a [Consumer] receives. A consumer could receive
//! all messages for a topic, or filter messages by subtopic.
//! When creating a consumer, a function or closure is passed which defines the rule for filtering subtopics.
//!
//! This is similar to the concept of a [`routing_keys`](https://www.rabbitmq.com/tutorials/tutorial-five-python#topic-exchange) in RabbitMQ,
//! except rather than using a string to filter the consumers messages, we use create arbitrary filtering rules passed as closures.
//! This provides both type safety and flexibility.
//!
//! ```rust
//! # use tokio_test;
//! # use lug::{EventBus, Topic, Filter};
//! struct SubTopics {
//!    a: u32,
//!    b: u32,
//! };
//!
//! impl Topic for SubTopics {
//!     type MessageType = &'static str;
//! }
//!
//! # tokio_test::block_on(async {
//! let (addr, handle) = EventBus::spawn(100);
//!
//! // These are two different topics
//! let topic_a = SubTopics { a: 1, b: 1 };
//! let topic_b = SubTopics { a: 1, b: 2 };
//!
//! let producer_1 = addr.producer(topic_a);
//! let producer_2 = addr.producer(topic_b);
//!
//! // consumer_a will receive all messages
//! let mut consumer_a = addr
//!     .consumer::<SubTopics>()
//!     .await
//!     .unwrap();
//!
//! // consumer_b will receive all messages where the topic.b == 2
//! let mut consumer_b = addr.consumer_with_filter(|t: &SubTopics| t.b == 2).await.unwrap();
//!
//! producer_1.send("This is subtopic_a").await.unwrap();
//! producer_2.send("This is subtopic_b").await.unwrap();
//!
//! assert_eq!(
//!     *consumer_a.recv().await.unwrap().as_ref(),
//!     "This is subtopic_a"
//! );
//! assert_eq!(
//!     *consumer_a.recv().await.unwrap().as_ref(),
//!     "This is subtopic_b"
//! );
//!
//! assert_eq!(
//!     *consumer_b.recv().await.unwrap().as_ref(),
//!     "This is subtopic_b"
//! );
//!
//! handle.graceful_shutdown().await.unwrap();
//! # })
//! ```
//!
//! ## Enum Subtopics
//!
//! You can also use enums to define subtopics. In this example there is a single type called `EnumSubTopic` which defines
//! subtopics as enum variants and associated data. Remember that while enum variants and data define subtopics, only the type
//! (in this case `EnumSubTopic`) defines the topic.
//!
//! ```rust
//! # use tokio_test;
//! # use lug::{EventBus, Topic, Filter};
//! # #[allow(dead_code)]
//! enum EnumSubTopic {
//!     A,
//!     B { a: &'static str },
//! }
//!
//! impl Topic for EnumSubTopic {
//!     type MessageType = u32;
//! }
//! # tokio_test::block_on(async {
//! # let (addr, _handle) = EventBus::spawn(100);
//!
//! let producer_a = addr.producer(EnumSubTopic::B {
//!     a: "subtopic of subtopic B",
//! });
//!
//! let mut consumer_topic_b = addr
//!     .consumer_with_filter(
//!         |t: &EnumSubTopic| matches!(t, EnumSubTopic::B { a } if a == &"subtopic of subtopic B"),
//!     )
//!     .await.unwrap();
//!
//! # let mut _consumer_topic_a = addr.consumer_with_filter(|t| matches!(t, EnumSubTopic::A)).await.unwrap();
//! producer_a.send(3).await.unwrap();
//! let event = consumer_topic_b.recv().await.unwrap();
//!
//! assert_eq!(*event.as_ref(), 3);
//! # });
//!
//! ```
//! ## Stream Consumer
//! A [Consumer] can be converted to a [futures::Stream].
//! ```
//! # use futures::Stream;
//! # use tokio_test;
//! # use lug::{EventBus, Topic, Filter};
//! # use futures_util::StreamExt;
//! # tokio_test::block_on(async {
//! # let (addr, _handle) = EventBus::spawn(100);
//! # struct HelloTopic;
//! # impl Topic for HelloTopic {
//! #     type MessageType = String;
//! # }
//! let expected_values = vec!["First Message", "Second Message"];
//!
//! // Create a consumer and convert it to a stream
//! let mut consumer = addr.consumer::<HelloTopic>().await
//!     .unwrap()
//!     .to_stream()
//!     .enumerate();
//! # let producer = addr.producer(HelloTopic);
//! # producer.send("First Message".to_string()).await.unwrap();
//! # producer.send("Second Message".to_string()).await.unwrap();
//!
//! let mut recovered_values = vec![];
//! while let Some((i, value)) = consumer.next().await {
//!     recovered_values.push(value.as_ref().to_owned());
//!     if i == expected_values.len() - 1 {
//!         break;
//!     }
//! }
//!
//! assert_eq!(recovered_values, expected_values);
//! # });
//! ```

mod actor;
mod event_bus;
pub use event_bus::{Consumer, EventBus, Filter, Producer, Topic};
