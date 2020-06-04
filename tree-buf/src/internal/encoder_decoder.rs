use crate::internal::encodings::varint::{size_for_varint, encode_varint_into};
use crate::prelude::*;

// REMEMBER: The reason this is not a trait is because of partial borrows.
// If this is a trait, you can't borrow both bytes and lens at the same time.
#[cfg(feature = "encode")]
pub struct EncoderStream<'a, O> {
    pub bytes: &'a mut Vec<u8>,
    pub lens: &'a mut Vec<usize>,
    pub options: &'a O,
}


#[cfg(feature = "encode")]
impl<'a, O: EncodeOptions> EncoderStream<'a, O> {
    pub fn new(bytes: &'a mut Vec<u8>, lens: &'a mut Vec<usize>, options: &'a O) -> Self {
        Self { bytes, lens, options }
    }

    // TODO: Not yet used
    pub fn restore_if_void<T: TypeId>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let restore = self.bytes.len();
        let id = f(self);
        if id == T::void() {
            self.bytes.drain(restore..);
        }
        id
    }
    // TODO: Not yet used
    pub fn reserve_and_encode_with_varint(&mut self, max: u64, f: impl FnOnce(&mut Self) -> u64) {
        let reserved = size_for_varint(max);
        let start = self.bytes.len();
        for _ in 0..reserved {
            self.bytes.push(0);
        }
        let end = self.bytes.len();
        let v = f(self);
        debug_assert!(v <= max);
        encode_varint_into(v, &mut self.bytes[start..end]);
    }

    // See also: a0b1e0fa-e33c-4bda-8141-d184a1160143
    // Duplicated code.
    pub fn encode_with_id<T: TypeId>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let type_index = self.bytes.len();
        self.bytes.push(0);
        let id = f(self);
        debug_assert!(id != T::void() || (self.bytes.len() == type_index + 1), "Expecting Void to encode no bytes to stream");
        self.bytes[type_index] = id.into();
        id
    }
    pub fn encode_with_len<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let start = self.bytes.len();
        let result = f(self);
        self.lens.push(self.bytes.len() - start);
        result
    }
}

#[cfg(feature = "encode")]
pub trait Encodable: Sized {
    type EncoderArray: EncoderArray<Self>;
    // At the root level, there is no need to buffer/flush, just encode. By not buffering, we may
    // significantly decrease total memory usage when there are multiple arrays at the root level,
    // by not requiring that both be fully buffered simultaneously.
    #[must_use]
    fn encode_root<O: EncodeOptions>(&self, stream: &mut EncoderStream<'_, O>) -> RootTypeId;
}

#[cfg(feature = "decode")]
pub trait Decodable: Sized {
    type DecoderArray: DecoderArray<Decode = Self>;
    fn decode(sticks: DynRootBranch<'_>, options: &impl DecodeOptions) -> DecodeResult<Self>;
}

// TODO: Introduce a separate "Scratch" type to make eg: EncoderArray re-usable.
// The scratch type would be passed to encode, so it needs to be for Encodable (root)
// Since not all root types have array children, some of these structs will be empty.
// To some degree it is possible to know about re-use for fields of the same type, reducing
// allocations further.

#[cfg(feature = "encode")]
pub trait EncoderArray<T: ?Sized>: Default {
    fn buffer<'a, 'b: 'a>(&'a mut self, value: &'b T);
    fn flush<O: EncodeOptions>(self, stream: &mut EncoderStream<'_, O>) -> ArrayTypeId;
}

#[cfg(feature = "decode")]
pub trait DecoderArray: Sized + Send {
    type Error: CoercibleWith<DecodeError> + CoercibleWith<Never>;
    type Decode;
    // TODO: It would be nice to be able to keep reference to the original byte array, especially for decoding strings.
    // I think that may require GAT though the way things are setup so come back to this later.
    fn new(sticks: DynArrayBranch<'_>, options: &impl DecodeOptions) -> DecodeResult<Self>;
    fn decode_next(&mut self) -> Result<Self::Decode, Self::Error>;
}

pub trait InfallibleDecoderArray: Sized {
    type Decode;
    /// This isn't actually infallable, it's just named this to not conflict.
    fn new_infallible(sticks: DynArrayBranch<'_>, options: &impl DecodeOptions) -> DecodeResult<Self>;
    fn decode_next_infallible(&mut self) -> Self::Decode;
}

/// This trait exists to first reduce a little bit of boilerplate for the common
/// case of not having fallibility, but also to automatically inline at least the Ok
/// wrapping portion of the code to aid the optimizer in knowing that the Error path
/// is impossible. Putting the inline here instead of on a decode_next of a DecoderArray
/// implementation allows for not necessarily inlining what may be a larger method.
/// It may not be necessary, but why not.
impl<T: InfallibleDecoderArray + Send> DecoderArray for T {
    type Decode = <Self as InfallibleDecoderArray>::Decode;
    type Error = Never;

    #[inline(always)]
    fn new(sticks: DynArrayBranch<'_>, options: &impl DecodeOptions) -> DecodeResult<Self> {
        InfallibleDecoderArray::new_infallible(sticks, options)
    }

    #[inline(always)]
    fn decode_next(&mut self) -> Result<Self::Decode, Self::Error> {
        Ok(InfallibleDecoderArray::decode_next_infallible(self))
    }
}