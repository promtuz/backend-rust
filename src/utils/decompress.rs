use std::io::Read;

use axum::body::Bytes;
use serde::de::DeserializeOwned;

pub fn decode_zlib_json<T: DeserializeOwned>(body: Bytes) -> Result<T, &'static str> {
  use flate2::read::ZlibDecoder;

  let mut decoder = ZlibDecoder::new(&body[..]);
  let mut decompressed = String::new();
  decoder.read_to_string(&mut decompressed).map_err(|_| "decompression failed")?;
  
  serde_json::from_str::<T>(&decompressed).map_err(|_| "invalid json")
}
