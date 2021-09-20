use pin_project_lite::pin_project;
use std::cell::RefCell;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::{fmt, thread};

// Reproduced from `tokio` (under MIT license) at https://github.com/tokio-rs/tokio/blob/9a3603fa75ff854e007d372061edf47cf8d02690/tokio/src/task/task_local.rs.

/// Declares a new task-local key.
///
/// # Syntax
///
/// The macro wraps any number of static declarations and makes them local to the current task.
/// Publicity and attributes for each static is preserved. For example:
///
#[macro_export]
macro_rules! task_local {
     // empty (base case for the recursion)
    () => {};

    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty; $($rest:tt)*) => {
        $crate::__task_local_inner!($(#[$attr])* $vis $name, $t);
        $crate::task_local!($($rest)*);
    };

    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty) => {
        $crate::__task_local_inner!($(#[$attr])* $vis $name, $t);
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! __task_local_inner {
    ($(#[$attr:meta])* $vis:vis $name:ident, $t:ty) => {
        $vis static $name: $crate::task_local::LocalKey<$t> = {
            std::thread_local! {
                static __KEY: std::cell::RefCell<Option<$t>> = std::cell::RefCell::new(None);
            }

            $crate::task_local::LocalKey { inner: __KEY }
        };
    };
}

/// A key for task-local data.
///
/// This type is generated by the `task_local!` macro.
///
/// Unlike [`std::thread::LocalKey`], `LocalKey` will
/// _not_ lazily initialize the value on first access. Instead, the
/// value is first initialized when the future containing
/// the task-local is first polled by a futures executor.
pub struct LocalKey<T: 'static> {
    #[doc(hidden)]
    pub inner: thread::LocalKey<RefCell<Option<T>>>,
}

#[cfg_attr(any(not(feature = "handle"), target_arch = "wasm32"), allow(unused))]
impl<T: 'static> LocalKey<T> {
    /// Sets a value `T` as the task-local value for the future `F`.
    ///
    /// On completion of `scope`, the task-local will be dropped.
    pub async fn scope<F>(&'static self, value: T, f: F) -> F::Output
    where
        F: Future,
    {
        TaskLocalFuture {
            local: self,
            slot: Some(value),
            future: f,
        }
        .await
    }

    /// Accesses the current task-local and runs the provided closure.
    ///
    /// # Panics
    ///
    /// This function will panic if not called within the context
    /// of a future containing a task-local with the corresponding key.
    pub fn with<F, R>(&'static self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        self.try_with(f).expect(
            "cannot access a Task Local Storage value \
             without setting it via `LocalKey::set`",
        )
    }

    /// Accesses the current task-local and runs the provided closure.
    ///
    /// If the task-local with the associated key is not present, this
    /// method will return an `AccessError`. For a panicking variant,
    /// see `with`.
    pub fn try_with<F, R>(&'static self, f: F) -> Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        self.inner.with(|v| {
            if let Some(val) = v.borrow().as_ref() {
                Ok(f(val))
            } else {
                Err(AccessError { _private: () })
            }
        })
    }
}

impl<T: 'static> fmt::Debug for LocalKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("LocalKey { .. }")
    }
}

pin_project! {
    struct TaskLocalFuture<T: StaticLifetime, F> {
        local: &'static LocalKey<T>,
        slot: Option<T>,
        #[pin]
        future: F,
    }
}

impl<T: 'static, F> TaskLocalFuture<T, F> {
    fn with_task<F2: FnOnce(Pin<&mut F>) -> R, R>(self: Pin<&mut Self>, f: F2) -> R {
        struct Guard<'a, T: 'static> {
            local: &'static LocalKey<T>,
            slot: &'a mut Option<T>,
            prev: Option<T>,
        }

        impl<T> Drop for Guard<'_, T> {
            fn drop(&mut self) {
                let value = self.local.inner.with(|c| c.replace(self.prev.take()));
                *self.slot = value;
            }
        }

        let mut project = self.project();
        let val = project.slot.take();

        let prev = project.local.inner.with(|c| c.replace(val));

        let _guard = Guard {
            prev,
            slot: &mut project.slot,
            local: *project.local,
        };

        f(project.future)
    }
}

impl<T: 'static, F: Future> Future for TaskLocalFuture<T, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.with_task(|f| f.poll(cx))
    }
}

// Required to make `pin_project` happy.
trait StaticLifetime: 'static {}
impl<T: 'static> StaticLifetime for T {}

/// An error returned by [`LocalKey::try_with`](method@LocalKey::try_with).
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct AccessError {
    _private: (),
}

impl fmt::Debug for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessError").finish()
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt("task-local value not set", f)
    }
}

impl Error for AccessError {}
