use std::{any::Any, collections::HashMap, marker::PhantomData};

use tokio::sync::{
    broadcast,
    mpsc::{self, UnboundedReceiver, UnboundedSender},
};

use crate::{
    channel::{ChannelRunner, Channel},
    types::Token,
};

#[derive(Debug, Clone)]
pub enum Message {
    Raw(String),
    Reply(String),
}

// pub trait PubSub {
//     fn subscribe(
//         &mut self,
//         key: &str,
//         reply_to: UnboundedChannel<Message>,
//     ) -> Result<(UnboundedSender<Message>, broadcast::Receiver<Message>), Error>;
// }

#[derive(Debug)]
pub enum Error {
    Subscribe,
    NoTopic,
}

pub struct Topic {
    broadcaster: broadcast::Sender<Message>,
    sender: UnboundedSender<Message>,
    receiver: UnboundedReceiver<Message>,
}

// Behavior will live in the parent Pubsub
impl Topic {
    pub fn new() -> Topic {
        let (broadcaster, _) = broadcast::channel(512);
        let (sender, receiver) = mpsc::unbounded_channel();

        Topic {
            broadcaster,
            sender,
            receiver,
        }
    }
}

pub struct ChannelTemplate<T: ChannelBehavior> {
    _t: PhantomData<T>,
}

pub struct LocalPubsub {
    topics: HashMap<String, Topic>,
    sockets: HashMap<Token, UnboundedSender<Message>>,
    // behaviors: HashMap<String, Box<dyn Any>>,
    behavior: Box<dyn ChannelBehavior>,
}

impl LocalPubsub {
    fn new(behavior: Box<dyn ChannelBehavior>) -> Self {
        LocalPubsub {
            topics: HashMap::new(),
            sockets: HashMap::new(),
            behavior,
        }
    }
}

impl LocalPubsub {
    fn subscribe(
        &mut self,
        key: &str,
        token: Token,
        socket_sender: UnboundedSender<Message>,
    ) -> Result<(UnboundedSender<Message>, broadcast::Receiver<Message>), Error> {
        let topic = self.topics.entry(key.to_string()).or_insert(Topic::new());

        self.sockets.entry(token).or_insert(socket_sender);
        Ok((topic.sender.clone(), topic.broadcaster.subscribe()))
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    #[derive(Clone, Debug)]
    struct TestBehavior;

    impl ChannelBehavior for TestBehavior {
        fn handle_message(
            &mut self,
            _message: &crate::message::MessageContext,
        ) -> Option<crate::message::Message> {
            None
        }

        fn handle_join(
            &mut self,
            _message: &crate::message::MessageContext,
        ) -> Result<(), crate::channel::JoinError> {
            Ok(())
        }
    }

    async fn test_subscribe() {
        let mut pubsub = LocalPubsub::new(Box::new(TestBehavior));
        let (local_sender, local_receiver) = unbounded_channel();
        let token = crate::get_token();
        let (sender, mut subscriber) = pubsub.subscribe("test_topic", token, local_sender).unwrap();
        let message = Message::Raw("foo".to_string());

        sender.send(message).unwrap();
        let received = subscriber.recv().await;
    }
}
