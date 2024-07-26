use std::{
    collections::{hash_map::Entry, HashMap},
    hash::Hash,
    mem::forget,
    pin::Pin,
    sync::{Arc, Weak},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bitvec::vec::BitVec;
use futures::{join, Stream};
use tokio::sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex,
};

use crate::{
    decoder::DelayDecoder,
    session::{delay_session, DelaySession, Signal, SignalSender},
};

type SharedSignalSenderMap<K> = Mutex<HashMap<K, SignalSender>>;

#[derive(Debug)]
pub struct DelaySessionStore<K> {
    timeout_duration: Duration,
    sender_map: Arc<SharedSignalSenderMap<K>>,
    result_sender: Sender<(K, BitVec)>,
}

impl<K> DelaySessionStore<K>
where
    K: Clone + Eq + Hash + Send + 'static,
{
    pub async fn push_signal<D>(
        &self,
        mut key: K,
        instant: Instant,
        mut decoder_factory: impl FnMut() -> D + Send + 'static,
    ) -> Result<(), ()>
    where
        D: DelayDecoder + Send + 'static,
    {
        match self.sender_map.lock().await.entry(key.clone()) {
            Entry::Occupied(entry) => {
                let sender = entry.get();
                sender
                    .send(Signal {
                        instant,
                        timeout_instant: instant + self.timeout_duration,
                    })
                    .await
                    .map_err(|_| ())
            }

            Entry::Vacant(entry) => {
                let (signal_sender, session) =
                    delay_session(decoder_factory(), instant, instant + self.timeout_duration);
                entry.insert(signal_sender);

                let sender_map = Arc::downgrade(&self.sender_map);
                let result_sender = self.result_sender.clone();

                tokio::spawn(async move {
                    struct UniqueSenderRemoveGuard<'a, K>
                    where
                        K: Clone + Eq + Hash + Send + 'static,
                    {
                        key: &'a mut K,
                        sender_map: Weak<SharedSignalSenderMap<K>>,
                    }

                    impl<K> Drop for UniqueSenderRemoveGuard<'_, K>
                    where
                        K: Clone + Eq + Hash + Send + 'static,
                    {
                        fn drop(&mut self) {
                            if let Some(map) = self.sender_map.upgrade() {
                                let key = self.key.clone();
                                tokio::spawn(async move {
                                    map.lock().await.remove(&key);
                                });
                            }
                        }
                    }

                    struct SharedSenderRemoveGuard<'a, K>
                    where
                        K: Clone + Eq + Hash + Send + 'static,
                    {
                        key: &'a K,
                        sender_map: Weak<SharedSignalSenderMap<K>>,
                    }

                    impl<K> Drop for SharedSenderRemoveGuard<'_, K>
                    where
                        K: Clone + Eq + Hash + Send + 'static,
                    {
                        fn drop(&mut self) {
                            if let Some(map) = self.sender_map.upgrade() {
                                let key = self.key.clone();
                                tokio::spawn(async move {
                                    map.lock().await.remove(&key);
                                });
                            }
                        }
                    }

                    let mut session = session;
                    loop {
                        let guard = UniqueSenderRemoveGuard {
                            key: &mut key,
                            sender_map: sender_map.clone(),
                        };

                        let (result, signal_receiver) = session.await;

                        forget(guard);

                        let guard = SharedSenderRemoveGuard {
                            key: &key,
                            sender_map: sender_map.clone(),
                        };

                        session =
                            DelaySession::start_with_receiver(decoder_factory(), signal_receiver);

                        if !session.is_open() {
                            if let Some(map) = sender_map.upgrade() {
                                let key_clone = key.clone();
                                forget(guard);
                                let key_mut = &mut key;
                                join!(
                                    async move {
                                        map.lock().await.remove(&*key_mut);
                                    },
                                    async move {
                                        let _ = result_sender.send((key_clone, result)).await;
                                    }
                                );
                            } else {
                                forget(guard);
                                let _ = result_sender.send((key, result)).await;
                            }
                            break;
                        } else {
                            let key_clone = key.clone();
                            forget(guard);
                            let guard = UniqueSenderRemoveGuard {
                                key: &mut key,
                                sender_map: sender_map.clone(),
                            };
                            let _ = result_sender.send((key_clone, result)).await;
                            forget(guard);
                        }
                    }
                });

                Ok(())
            }
        }
    }
}

#[derive(Debug)]
pub struct DelaySessionStream<K> {
    receiver: Receiver<(K, BitVec)>,
}

impl<K> Stream for DelaySessionStream<K> {
    type Item = (K, BitVec);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

pub fn delay_session_store<K>(
    timeout_duration: Duration,
) -> (DelaySessionStore<K>, DelaySessionStream<K>) {
    let (sender, receiver) = channel(8);

    (
        DelaySessionStore {
            timeout_duration,
            sender_map: Default::default(),
            result_sender: sender,
        },
        DelaySessionStream { receiver },
    )
}
