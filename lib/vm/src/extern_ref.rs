use std::any::Any;

use wasmer_types::RawValue;

use crate::context::InternalContextHandle;

/// Underlying object referenced by a `VMExternRef`.
pub struct VMExternObj {
    contents: Box<dyn Any + Send + Sync + 'static>,
}

impl VMExternObj {
    /// Wraps the given value to expose it to Wasm code as an externref.
    pub fn new(val: impl Any + Send + Sync + 'static) -> Self {
        Self {
            contents: Box::new(val),
        }
    }

    /// Returns a reference to the underlying value.
    pub fn as_ref(&self) -> &(dyn Any + Send + Sync + 'static) {
        &*self.contents
    }
}

/// Represents an opaque reference to any data within WebAssembly.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct VMExternRef(pub InternalContextHandle<VMExternObj>);

impl VMExternRef {
    /// Converts the `VMExternRef` into a `RawValue`.
    pub fn into_raw(self) -> RawValue {
        RawValue {
            funcref: self.0.index(),
        }
    }

    /// Extracts a `VMExternRef` from a `RawValue`.
    pub unsafe fn from_raw(raw: RawValue) -> Option<Self> {
        InternalContextHandle::from_index(raw.externref).map(Self)
    }
}
