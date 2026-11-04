use crate::{DecodeError, Image, Limits};

pub fn decode(_d: &[u8], _l: &Limits) -> Result<Image, DecodeError> {
    Err(DecodeError::Unsupported("todo"))
}
