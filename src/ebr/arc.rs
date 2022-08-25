use super::underlying::Underlying;
use super::{Barrier, Collectible, Ptr};

use std::ops::Deref;
use std::ptr::{addr_of, NonNull};

/// [`Arc`] is a reference-counted handle to an instance.
#[derive(Debug)]
pub struct Arc<T: 'static> {
    instance_ptr: NonNull<Underlying<T>>,
}

impl<T: 'static> Arc<T> {
    /// Creates a new instance of [`Arc`].
    ///
    /// # Examples
    ///
    /// ```
    /// use scc::ebr::Arc;
    ///
    /// let arc: Arc<usize> = Arc::new(31);
    /// ```
    #[inline]
    pub fn new(t: T) -> Arc<T> {
        let boxed = Box::new(Underlying::new(t));
        Arc {
            instance_ptr: unsafe { NonNull::new_unchecked(Box::into_raw(boxed)) },
        }
    }

    /// Generates a [`Ptr`] out of the [`Arc`].
    ///
    /// # Examples
    ///
    /// ```
    /// use scc::ebr::{Arc, Barrier};
    ///
    /// let arc: Arc<usize> = Arc::new(37);
    /// let barrier = Barrier::new();
    /// let ptr = arc.ptr(&barrier);
    /// assert_eq!(*ptr.as_ref().unwrap(), 37);
    /// ```
    #[inline]
    #[must_use]
    pub fn ptr<'b>(&self, _barrier: &'b Barrier) -> Ptr<'b, T> {
        Ptr::from(self.instance_ptr.as_ptr())
    }

    /// Returns a mutable reference to the underlying instance if the instance is exclusively
    /// owned.
    ///
    /// # Safety
    ///
    /// The method is `unsafe` since there can be a [`Ptr`] to the instance; in other words, it is
    /// safe as long as there is no [`Ptr`] to the instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use scc::ebr::Arc;
    ///
    /// let mut arc: Arc<usize> = Arc::new(38);
    /// unsafe {
    ///     *arc.get_mut().unwrap() += 1;
    /// }
    /// assert_eq!(*arc, 39);
    /// ```
    #[inline]
    pub unsafe fn get_mut(&mut self) -> Option<&mut T> {
        self.instance_ptr.as_mut().get_mut()
    }

    /// Provides a raw pointer to the underlying instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use scc::ebr::Arc;
    /// use std::sync::atomic::AtomicBool;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let arc: Arc<usize> = Arc::new(10);
    /// let arc_cloned: Arc<usize> = arc.clone();
    ///
    /// assert_eq!(arc.as_ptr(), arc_cloned.as_ptr());
    /// assert_eq!(unsafe { *arc.as_ptr() }, unsafe { *arc_cloned.as_ptr() });
    /// ```
    #[inline]
    #[must_use]
    pub fn as_ptr(&self) -> *const T {
        addr_of!(**self.underlying())
    }

    /// Drops the underlying instance if the last reference is dropped.
    ///
    /// The instance is not passed to the garbage collector when the last reference is dropped,
    /// instead the method drops the instance immediately. The semantics is the same as that of
    /// [`std::sync::Arc`].
    ///
    /// # Safety
    ///
    /// The caller must ensure that there is no [`Ptr`] pointing to the instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use scc::ebr::Arc;
    /// use std::sync::atomic::AtomicBool;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// static DROPPED: AtomicBool = AtomicBool::new(false);
    /// struct T(&'static AtomicBool);
    /// impl Drop for T {
    ///     fn drop(&mut self) {
    ///         self.0.store(true, Relaxed);
    ///     }
    /// }
    ///
    /// let arc: Arc<T> = Arc::new(T(&DROPPED));
    ///
    /// unsafe {
    ///     arc.drop_in_place();
    /// }
    /// assert!(DROPPED.load(Relaxed));
    /// ```
    #[inline]
    pub unsafe fn drop_in_place(mut self) {
        if self.underlying().drop_ref() {
            self.instance_ptr.as_mut().drop_and_free();
            std::mem::forget(self);
        }
    }

    /// Provides a raw pointer to its [`Underlying`].
    #[inline]
    pub(super) fn as_underlying_ptr(&self) -> *mut Underlying<T> {
        self.instance_ptr.as_ptr()
    }

    /// Creates a new [`Arc`] from the given pointer.
    #[inline]
    pub(super) fn from(ptr: NonNull<Underlying<T>>) -> Arc<T> {
        debug_assert_ne!(
            unsafe {
                ptr.as_ref()
                    .ref_cnt()
                    .load(std::sync::atomic::Ordering::Relaxed)
            },
            0
        );
        Arc { instance_ptr: ptr }
    }

    /// Drops the reference, and returns the underlying pointer if the last reference was
    /// dropped.
    #[inline]
    pub(super) fn drop_ref(&self) -> Option<*mut Underlying<T>> {
        if self.underlying().drop_ref() {
            Some(self.instance_ptr.as_ptr())
        } else {
            None
        }
    }

    /// Returns a reference to the underlying instance.
    #[inline]
    fn underlying(&self) -> &Underlying<T> {
        unsafe { self.instance_ptr.as_ref() }
    }
}

impl<T: 'static> AsRef<T> for Arc<T> {
    #[inline]
    fn as_ref(&self) -> &T {
        &**self.underlying()
    }
}

impl<T: 'static> Clone for Arc<T> {
    #[inline]
    fn clone(&self) -> Self {
        debug_assert_ne!(
            self.underlying()
                .ref_cnt()
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        self.underlying().add_ref();
        Self {
            instance_ptr: self.instance_ptr,
        }
    }
}

impl<T: 'static> Deref for Arc<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &**self.underlying()
    }
}

impl<T: 'static> Drop for Arc<T> {
    #[inline]
    fn drop(&mut self) {
        if self.underlying().drop_ref() {
            let barrier = Barrier::new();
            barrier.collect(self.instance_ptr.as_ptr());
        }
    }
}

impl<'b, T: 'static> TryFrom<Ptr<'b, T>> for Arc<T> {
    type Error = Ptr<'b, T>;

    #[inline]
    fn try_from(ptr: Ptr<'b, T>) -> Result<Self, Self::Error> {
        if let Some(arc) = ptr.get_arc() {
            Ok(arc)
        } else {
            Err(ptr)
        }
    }
}

unsafe impl<T: 'static + Send> Send for Arc<T> {}
unsafe impl<T: 'static + Sync> Sync for Arc<T> {}
