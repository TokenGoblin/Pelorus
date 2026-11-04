use crate::{DecodeError, Decoded, Limits};

pub fn decode(_d: &[u8], _l: &Limits) -> Result<Decoded, DecodeError> {
    Err(DecodeError::Unsupported("todo"))
}
