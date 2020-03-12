use crate::prelude::*;
use num_traits::AsPrimitive as _;
use std::convert::TryInto;
use std::mem::size_of;
use std::vec::IntoIter;
// TODO: Zfp See also 6669608f-1441-4bdb-97c0-5260c7c4bf0f
//use ndarray_zfp_rs::Zfp;

// Promising Compressors:
// Gorilla - https://crates.io/crates/tsz   http://www.vldb.org/pvldb/vol8/p1816-teller.pdf
// FPC
// Akamuli - https://akumuli.org/akumuli/2017/02/05/compression_part2/
// ? http://blog.omega-prime.co.uk/2016/01/25/compression-of-floating-point-timeseries/
// https://www.cs.unc.edu/~isenburg/lcpfpv/
// dfcm - https://userweb.cs.txstate.edu/~mb92/papers/dcc06.pdf

// TODO: Lowerings
// Interesting reading: https://internals.rust-lang.org/t/tryfrom-for-f64/9793/35
// A useful starting point is that all possible down-cast through up-cast round trips
// must preserve bit-for-bit the original value. That's not quite enough though, since this
// is true for some values due to saturating rounding that one wouldn't want to downcast.
// https://floating-point-gui.de/formats/fp/
// f64 -> u64
// f64 -> f32
// f32 -> u32

// TODO: More compressors

macro_rules! impl_float {
    ($T:ident, $write_item:ident, $read_item:ident, $id:ident, $fixed:ident, $zfp:ident, $($rest:ident),*) => {
        // TODO: Check for lowering - f64 -> f63
        #[cfg(feature = "write")]
        fn $write_item(item: $T, bytes: &mut Vec<u8>) {
            let b = item.to_le_bytes();
            bytes.extend_from_slice(&b);
        }

        #[cfg(feature = "read")]
        fn $read_item(bytes: &[u8], offset: &mut usize) -> ReadResult<$T> {
            let bytes = read_bytes(size_of::<$T>(), bytes, offset)?;
            // This unwrap is ok, because we just read exactly size_of::<T> bytes on the line above.
            Ok(<$T>::from_le_bytes(bytes.try_into().unwrap()))
        }


        #[cfg(feature = "write")]
        impl<'a> Writable<'a> for $T {
            type WriterArray = Vec<$T>;
            fn write_root<'b: 'a>(&'b self, stream: &mut impl WriterStream) -> RootTypeId {
                let value = *self;

                // Check for positive sign so that -0.0 goes through
                // the unhappy path but round-trips bit-for-bit
                if value == 0.0 && value.is_sign_positive() {
                    RootTypeId::Zero
                } else if value == 1.0 {
                    RootTypeId::One
                } else if value == -1.0 {
                    RootTypeId::NegOne
                } else if value.is_nan() {
                    // FIXME: Check for canonical NaN,
                    // so that other NaN round trip bit-for-bit
                    RootTypeId::NaN
                } else {
                    $write_item(value, stream.bytes());
                    RootTypeId::$id
                }
            }
        }


        #[cfg(feature = "read")]
        impl Readable for $T {
            type ReaderArray = IntoIter<$T>;
            fn read(sticks: DynRootBranch<'_>) -> ReadResult<Self> {
                match sticks {
                    DynRootBranch::Integer(root_integer) => {
                        // FIXME: Fast and lose to get refactoring done. Double check here.
                        // Also, float can express some (but not all) integers larger than MAX_SAFE_INT
                        match root_integer {
                            RootInteger::U(u) => {
                                if u < (2 << std::$T::MANTISSA_DIGITS) {
                                    Ok(u as $T)
                                } else {
                                    Err(ReadError::SchemaMismatch)
                                }
                            }
                            RootInteger::S(s) => {
                                if s < (2 << std::$T::MANTISSA_DIGITS) && s > (-2 << (std::$T::MANTISSA_DIGITS - 1)) {
                                    // FIXME: Made up number
                                    Ok(s as $T)
                                } else {
                                    Err(ReadError::SchemaMismatch)
                                }
                            }
                        }
                    }
                    DynRootBranch::Float(root_float) => {
                        match root_float {
                            // FIXME: Macro here - should be schema mismatch for f64 -> f32
                            RootFloat::F64(v) => Ok(v as $T),
                            RootFloat::NaN => Ok(std::$T::NAN),
                            // This should be safe to cast without loss of information.
                            // Double-check that the meaning of various NaN values
                            // is preserved though (signaling, non-signaling, etc)
                            // https://stackoverflow.com/a/59795029/11837266
                            RootFloat::F32(v) => Ok(v as $T),
                        }
                    }
                    _ => Err(ReadError::SchemaMismatch),
                }
            }
        }


        #[cfg(feature = "read")]
        impl ReaderArray for IntoIter<$T> {
            type Read = $T;
            fn new(sticks: DynArrayBranch<'_>) -> ReadResult<Self> {
                match sticks {
                    DynArrayBranch::Float(float) => {
                        match float {
                            ArrayFloat::F64(bytes) => {
                                // FIXME: Should do schema mismatch for f32 -> f64
                                let values = read_all(bytes, |bytes, offset| Ok(read_64(bytes, offset)?.as_()))?;
                                Ok(values.into_iter())
                            }
                            ArrayFloat::F32(bytes) => {
                                let values = read_all(bytes, |bytes, offset| Ok(read_32(bytes, offset)?.as_()))?;
                                Ok(values.into_iter())
                            },
                            ArrayFloat::DoubleGorilla(bytes) => {
                                // FIXME: Should do schema mismatch for f32 -> f64
                                let num_bits_last_elm = bytes.last().ok_or_else(|| ReadError::InvalidFormat(InvalidFormat::DecompressionError))?;
                                let bytes = &bytes[..bytes.len()-1];
                                let last = &bytes[bytes.len()-(bytes.len() % 8)..];
                                let bytes = &bytes[..bytes.len() - last.len()];
                                let mut last_2 = [0u8; 8];
                                for (i, value) in last.iter().enumerate() {
                                    last_2[i+(8-last.len())] = *value;
                                }
                                let last = u64::from_le_bytes(last_2);
                                // TODO: Change this to check that num_bits_last_elm is correct
                                if bytes.len() % size_of::<u64>() != 0 {
                                    return Err(ReadError::InvalidFormat(InvalidFormat::DecompressionError));
                                }
                                // TODO: (Performance) The following can use unchecked, since we just verified the size is valid.
                                let mut data = read_all(bytes, |bytes, offset| {
                                    let start = *offset;
                                    let end = start + size_of::<u64>();
                                    let le_bytes = &bytes[start..end];
                                    *offset = end;
                                    let result = u64::from_le_bytes(le_bytes.try_into().unwrap());
                                    Ok(result)
                                })?;
                                data.push(last);
                                let reader = gibbon::vec_stream::VecReader::new(&data, *num_bits_last_elm);
                                let iterator = gibbon::DoubleStreamIterator::new(reader);
                                // FIXME: It seems like this collect can panic if the data is invalid.
                                let values: Vec<_> = iterator.map(|v| v.as_()).collect();
                                Ok(values.into_iter())
                            }
                        }
                    }
                    // TODO: There are some conversions that are infallable.
                    // Eg: Simple16.
                    _ => Err(ReadError::SchemaMismatch),
                }
            }
            fn read_next(&mut self) -> ReadResult<Self::Read> {
                self.next().ok_or_else(|| ReadError::InvalidFormat(InvalidFormat::ShortArray))
            }
        }

        #[cfg(feature = "write")]
        impl<'a> WriterArray<'a> for Vec<$T> {
            type Write = $T;
            fn buffer<'b: 'a>(&mut self, value: &'b Self::Write) {
                self.push(*value);
            }
            fn flush(self, stream: &mut impl WriterStream) -> ArrayTypeId {
                let compressors: Vec<Box<dyn Compressor<Data=$T>>> = vec![
                    Box::new($fixed),
                    $(Box::new($rest)),*
                ];
                // TODO: Zfp See also 6669608f-1441-4bdb-97c0-5260c7c4bf0f
                /*
                if let Some(tolerance) = options.lossy_float_tolerance() {
                    compressors.push(Box::new($zfp { tolerance }));
                }
                */
                stream.write_with_len(|stream| compress(&self, stream.bytes(), &compressors[..]))
            }
        }

        struct $fixed;
        impl Compressor<'_> for $fixed {
            type Data=$T;
            fn fast_size_for(&self, data: &[Self::Data]) -> Option<usize> {
                Some(size_of::<$T>() * data.len())
            }
            fn compress(&self, data: &[Self::Data], bytes: &mut Vec<u8>) -> Result<ArrayTypeId, ()> {
                for item in data {
                    $write_item(*item, bytes);
                }
                Ok(ArrayTypeId::$id)
            }
        }

        // TODO: Zfp See also 6669608f-1441-4bdb-97c0-5260c7c4bf0f
        /*
        struct $zfp {
            tolerance: f64,
        }

        impl Compressor<'_> for $zfp {
            type Data=$T;
            fn compress(&self, data: &[Self::Data], bytes: &mut Vec<u8>) -> Result<ArrayTypeId, ()> {
                // FIXME: This is terrible. Consider using zfp-sys directly
                // Problems are needing copy of the data, needing to copy bytes again,
                // the header storing redundant information.
                let copy: Vec<_> = data.iter().copied().collect();
                let arr = ndarray::Array1::from(copy);
                let bin = arr.zfp_compress_with_header(self.tolerance);
                let bin = if let Ok(bin) = bin { bin } else { return Err(()) };
                bytes.extend_from_slice(&bin);
                Ok(ArrayTypeId::Void) // FIXME
            }
        }
        */

    };
}

impl_float!(f64, write_64, read_64, F64, Fixed64Compressor, ZfpCompressor64, GorillaCompressor);
impl_float!(f32, write_32, read_32, F32, Fixed32Compressor, ZfpCompressor32,);

// FIXME: Not clear if this is canon. The source for gibbon is a bit shaky.
// Alternatively, there is the tsz crate, but that doesn't offer a separate
// double-stream (just joined time+double stream). Both of the implementations
// aren't perfect for our API.
struct GorillaCompressor;
impl Compressor<'_> for GorillaCompressor {
    type Data = f64;
    fn compress(&self, data: &[Self::Data], bytes: &mut Vec<u8>) -> Result<ArrayTypeId, ()> {
        use gibbon::{vec_stream::VecWriter, DoubleStream};
        if data.is_empty() {
            return Ok(ArrayTypeId::DoubleGorilla);
        }

        let mut writer = VecWriter::new();
        let mut stream = DoubleStream::new();
        for value in data {
            stream.push(*value, &mut writer);
        }
        let VecWriter {
            mut bit_vector,
            used_bits_last_elm,
        } = writer;
        let last = bit_vector.pop().unwrap(); // Does not panic because of early out
                                              // TODO: It should be safe to do 1 extend and a transmute on le platforms
        for value in bit_vector {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let mut byte_count = used_bits_last_elm / 8;
        if byte_count * 8 != used_bits_last_elm {
            byte_count += 1;
        }
        let last = &(&last.to_le_bytes())[(8 - byte_count) as usize..];
        bytes.extend_from_slice(&last);
        bytes.push(used_bits_last_elm);
        Ok(ArrayTypeId::DoubleGorilla)
    }
}
