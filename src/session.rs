use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use bitvec::vec::BitVec;
use pin_project::pin_project;
use tokio::{
    sync::mpsc::{channel, Receiver, Sender},
    time::{sleep_until, Sleep},
};

use crate::decoder::DelayDecoder;

pub type SignalSender = Sender<Signal>;
pub type SignalReceiver = Receiver<Signal>;

pub fn signal_channel() -> (SignalSender, SignalReceiver) {
    channel(8)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Signal {
    pub instant: Instant,
    pub timeout_instant: Instant,
}

#[derive(Debug)]
#[pin_project]
pub struct DelaySession<D> {
    #[pin]
    inner: DelaySessionInner<D>,
}

impl<D> DelaySession<D> {
    pub fn new(
        decoder: D,
        receiver: SignalReceiver,
        start_instant: Instant,
        timeout_instant: Instant,
    ) -> Self {
        Self {
            inner: DelaySessionInner::Open {
                decoder,
                receiver,
                last_signal_instant: start_instant,
                timeout_sleep: sleep_until(timeout_instant.into()),
            },
        }
    }

    pub fn start_with_receiver(decoder: D, mut receiver: SignalReceiver) -> Self {
        match receiver.try_recv() {
            Ok(signal) => Self {
                inner: DelaySessionInner::Open {
                    decoder,
                    receiver,
                    last_signal_instant: signal.instant,
                    timeout_sleep: sleep_until(signal.timeout_instant.into()),
                },
            },
            Err(_) => Self {
                inner: DelaySessionInner::Closed,
            },
        }
    }

    pub const fn is_open(&self) -> bool {
        matches!(self.inner, DelaySessionInner::Open { .. })
    }
}

impl<D> Future for DelaySession<D>
where
    D: DelayDecoder,
{
    type Output = (BitVec, SignalReceiver);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx)
    }
}

pub fn delay_session<D>(
    decoder: D,
    start_instant: Instant,
    timeout_instant: Instant,
) -> (SignalSender, DelaySession<D>) {
    let (sender, receiver) = signal_channel();
    (
        sender,
        DelaySession::new(decoder, receiver, start_instant, timeout_instant),
    )
}

#[derive(Debug)]
#[pin_project(project = DelaySessionInnerProj, project_replace = DelaySessionInnerOwnedProj)]
enum DelaySessionInner<D> {
    Open {
        decoder: D,
        receiver: SignalReceiver,
        last_signal_instant: Instant,
        #[pin]
        timeout_sleep: Sleep,
    },
    Closed,
}

impl<D> Future for DelaySessionInner<D>
where
    D: DelayDecoder,
{
    type Output = (BitVec, SignalReceiver);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        fn close_assert_open<D>(session: Pin<&mut DelaySessionInner<D>>) -> (BitVec, SignalReceiver)
        where
            D: DelayDecoder,
        {
            match session.project_replace(DelaySessionInner::Closed) {
                DelaySessionInnerOwnedProj::Open {
                    decoder, receiver, ..
                } => (decoder.close(), receiver),
                DelaySessionInnerOwnedProj::Closed => unreachable!(),
            }
        }

        match self.as_mut().project() {
            DelaySessionInnerProj::Open {
                decoder,
                receiver,
                last_signal_instant,
                mut timeout_sleep,
            } => {
                if let Poll::Ready(signal_option) = receiver.poll_recv(cx) {
                    if let Some(signal) = signal_option {
                        if signal.instant >= timeout_sleep.deadline().into() {
                            return Poll::Ready(close_assert_open(self));
                        }

                        let mut new_timeout_instant = signal.timeout_instant;
                        decoder.push_duration(signal.instant - *last_signal_instant);
                        *last_signal_instant = signal.instant;

                        while let Poll::Ready(signal_option) = receiver.poll_recv(cx) {
                            if let Some(signal) = signal_option {
                                if signal.instant >= new_timeout_instant {
                                    return Poll::Ready(close_assert_open(self));
                                }

                                new_timeout_instant = signal.timeout_instant;
                                decoder.push_duration(signal.instant - *last_signal_instant);
                                *last_signal_instant = signal.instant;
                            } else {
                                return Poll::Ready(close_assert_open(self));
                            }
                        }

                        timeout_sleep.as_mut().reset(new_timeout_instant.into());
                    } else {
                        return Poll::Ready(close_assert_open(self));
                    }
                }

                if timeout_sleep.poll(cx).is_ready() {
                    Poll::Ready(close_assert_open(self))
                } else {
                    Poll::Pending
                }
            }
            DelaySessionInnerProj::Closed => {
                panic!("DelaySession polled after having returned Poll::Ready")
            }
        }
    }
}
