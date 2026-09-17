pub mod codec;
pub mod error;
pub mod pcf2;
pub mod varint;

pub use codec::{DecodeConfig, EncodeConfig, PdfStats, decode_async, decode_bytes, encode_bytes, encode_bytes_with_stats};
pub use error::{Error, Result};
pub use pcf2::{PCF2_MAGIC, PCF2_VERSION, Pcf2Decoder, Pcf2File, Pcf2Header, Pcf2Segment, Pcf2SegmentReader, SegmentKind};
