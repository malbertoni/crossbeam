extern crate coco;
extern crate crossbeam;
extern crate rand;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

pub use err::{RecvError, RecvTimeoutError, SendError, SendTimeoutError, TryRecvError, TrySendError};
pub use select::Select;

mod actor;
mod err;
mod flavors;
mod watch;
mod select;

// TODO: iterators
// TODO: Runtime selection checking (mark every participating tx/rx with current thread's selection id and index in the list)
// TODO: Thread notification in spinning loops (with is_done) (CAS to id to tell which channel is ready)
// TODO: Add Success state to selection. Or maybe start looping from scratch with a new random `start`?
// TODO: Use xorshift generator?
// TODO: The IsReady check must also check for closing (same in *_until methods)
// TODO: Panic if two selects are running at the same time
// TODO: Write CSP examples
// TODO: Use parking_lot?
// TODO: Use CachePadded
// TODO: select with recv & send on the same channel (all flavors) should work. Perhaps notify_one() must skip the current thread

enum Flavor<T> {
    Array(flavors::array::Queue<T>),
    List(flavors::list::Queue<T>),
    Zero(flavors::zero::Queue<T>),
}

struct Queue<T> {
    senders: AtomicUsize,
    receivers: AtomicUsize,
    flavor: Flavor<T>,
}

pub struct Sender<T>(Arc<Queue<T>>);

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}

impl<T> Sender<T> {
    fn new(q: Arc<Queue<T>>) -> Self {
        q.senders.fetch_add(1, SeqCst);
        Sender(q)
    }

    pub(crate) fn id(&self) -> usize {
        match self.0.flavor {
            Flavor::Array(ref q) => q.id(),
            Flavor::List(ref q) => q.id(),
            Flavor::Zero(ref q) => q.id(),
        }
    }

    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        match self.0.flavor {
            Flavor::Array(ref q) => q.try_send(value),
            Flavor::List(ref q) => q.try_send(value),
            Flavor::Zero(ref q) => q.try_send(value),
        }
    }

    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        let res = match self.0.flavor {
            Flavor::Array(ref q) => q.send_until(value, None),
            Flavor::List(ref q) => q.send_until(value, None),
            Flavor::Zero(ref q) => q.send_until(value, None),
        };
        match res {
            Ok(()) => Ok(()),
            Err(SendTimeoutError::Disconnected(v)) => Err(SendError(v)),
            Err(SendTimeoutError::Timeout(v)) => Err(SendError(v)),
        }
    }

    pub fn send_timeout(&self, value: T, dur: Duration) -> Result<(), SendTimeoutError<T>> {
        let deadline = Some(Instant::now() + dur);
        match self.0.flavor {
            Flavor::Array(ref q) => q.send_until(value, deadline),
            Flavor::List(ref q) => q.send_until(value, deadline),
            Flavor::Zero(ref q) => q.send_until(value, deadline),
        }
    }

    pub fn len(&self) -> usize {
        match self.0.flavor {
            Flavor::Array(ref q) => q.len(),
            Flavor::List(ref q) => q.len(),
            Flavor::Zero(ref q) => 0,
        }
    }

    // `true` if `try_send` would fail with `TrySendErr::Full(_)`
    pub fn is_full(&self) -> bool {
        match self.0.flavor {
            Flavor::Array(ref q) => q.len() == q.capacity(),
            Flavor::List(ref q) => false,
            Flavor::Zero(ref q) => !q.has_receivers(),
        }
    }

    pub fn is_disconnected(&self) -> bool {
        match self.0.flavor {
            Flavor::Array(ref q) => q.is_closed(),
            Flavor::List(ref q) => q.is_closed(),
            Flavor::Zero(ref q) => q.is_closed(),
        }
    }

    pub fn capacity(&self) -> Option<usize> {
        match self.0.flavor {
            Flavor::Array(ref q) => Some(q.capacity()),
            Flavor::List(ref q) => None,
            Flavor::Zero(ref q) => Some(0),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.0.senders.fetch_sub(1, SeqCst) == 1 {
            match self.0.flavor {
                Flavor::Array(ref q) => q.close(),
                Flavor::List(ref q) => q.close(),
                Flavor::Zero(ref q) => q.close(),
            };
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender::new(self.0.clone())
    }
}

pub struct Receiver<T>(Arc<Queue<T>>);

unsafe impl<T: Send> Send for Receiver<T> {}
unsafe impl<T: Send> Sync for Receiver<T> {}

impl<T> Receiver<T> {
    fn new(q: Arc<Queue<T>>) -> Self {
        q.receivers.fetch_add(1, SeqCst);
        Receiver(q)
    }

    pub(crate) fn id(&self) -> usize {
        match self.0.flavor {
            Flavor::Array(ref q) => q.id(),
            Flavor::List(ref q) => q.id(),
            Flavor::Zero(ref q) => q.id(),
        }
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match self.0.flavor {
            Flavor::Array(ref q) => q.try_recv(),
            Flavor::List(ref q) => q.try_recv(),
            Flavor::Zero(ref q) => q.try_recv(),
        }
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        let res = match self.0.flavor {
            Flavor::Array(ref q) => q.recv_until(None),
            Flavor::List(ref q) => q.recv_until(None),
            Flavor::Zero(ref q) => q.recv_until(None),
        };
        if let Ok(v) = res {
            Ok(v)
        } else {
            Err(RecvError)
        }
    }

    pub fn recv_timeout(&self, dur: Duration) -> Result<T, RecvTimeoutError> {
        let deadline = Some(Instant::now() + dur);
        match self.0.flavor {
            Flavor::Array(ref q) => q.recv_until(deadline),
            Flavor::List(ref q) => q.recv_until(deadline),
            Flavor::Zero(ref q) => q.recv_until(deadline),
        }
    }

    pub fn len(&self) -> usize {
        match self.0.flavor {
            Flavor::Array(ref q) => q.len(),
            Flavor::List(ref q) => q.len(),
            Flavor::Zero(ref q) => 0,
        }
    }

    // `true` if `try_recv` would fail with `TryRecvError::Empty`
    pub fn is_empty(&self) -> bool {
        match self.0.flavor {
            Flavor::Array(ref q) => q.len() == 0,
            Flavor::List(ref q) => q.len() == 0,
            Flavor::Zero(ref q) => !q.has_senders(),
        }
    }

    pub fn is_disconnected(&self) -> bool {
        match self.0.flavor {
            Flavor::Array(ref q) => q.is_closed(),
            Flavor::List(ref q) => q.is_closed(),
            Flavor::Zero(ref q) => q.is_closed(),
        }
    }

    pub fn capacity(&self) -> Option<usize> {
        match self.0.flavor {
            Flavor::Array(ref q) => Some(q.capacity()),
            Flavor::List(ref q) => None,
            Flavor::Zero(ref q) => Some(0),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if self.0.receivers.fetch_sub(1, SeqCst) == 1 {
            match self.0.flavor {
                Flavor::Array(ref q) => q.close(),
                Flavor::List(ref q) => q.close(),
                Flavor::Zero(ref q) => q.close(),
            };
        }
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Receiver::new(self.0.clone())
    }
}

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    let q = Arc::new(Queue {
        senders: AtomicUsize::new(0),
        receivers: AtomicUsize::new(0),
        flavor: Flavor::List(flavors::list::Queue::new()),
    });
    (Sender::new(q.clone()), Receiver::new(q))
}

pub fn bounded<T>(size: usize) -> (Sender<T>, Receiver<T>) {
    let q = Arc::new(Queue {
        senders: AtomicUsize::new(0),
        receivers: AtomicUsize::new(0),
        flavor: if size == 0 {
            Flavor::Zero(flavors::zero::Queue::new())
        } else {
            Flavor::Array(flavors::array::Queue::with_capacity(size))
        },
    });
    (Sender::new(q.clone()), Receiver::new(q))
}
