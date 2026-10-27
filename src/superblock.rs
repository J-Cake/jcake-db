use nom::bytes::*;
use nom::combinator::*;
use nom::number::le_u32;
use nom::{IResult, Parser};
use nom::error::ErrorKind;
use nom::sequence::preceded;
use uuid::Uuid;

pub(crate) struct Superblock {
    uuid: Uuid,
    version: u32,
    block_size: u32,
    block_offset: u32,
    crc: u32,
}

impl Superblock {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, crate::Error> {
        match Superblock::parse(bytes) {
            Ok((_, block)) => Ok(block),
            Err(err) => Err(crate::Error::new(err.to_string())),
        }
    }

    fn parse(input: &[u8]) -> IResult<&[u8], Self> {
        let (input, uuid) = map(preceded(tag(&b"fsdb"[..]), take(16usize)), |bytes| Uuid::from_slice(bytes))
            .parse(input)?;

        let uuid = uuid.map_err(|err| nom::Err::Error(nom::error::Error::new(input, ErrorKind::IsNot)))?;

        let (input, (version, block_size, block_offset, crc)) = (le_u32(), le_u32(), le_u32(), le_u32())
            .parse(input)?;

        Ok((input, Self {
            uuid,
            version,
            block_size,
            block_offset,
            crc
        }))
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut vec = Vec::with_capacity(28);
        vec.extend_from_slice(&b"fsdb"[..]);
        vec.extend_from_slice(self.uuid.as_bytes());
        vec.extend_from_slice(&self.version.to_le_bytes()[..]);
        vec.extend_from_slice(&self.block_size.to_le_bytes()[..]);

        return vec;
    }

    pub(crate) fn blank() -> Superblock {
        Self {
            uuid: Uuid::new_v4(),
            version: 0,
            block_size: 0,
            block_offset: 0,
            crc: 0,
        }
    }
}