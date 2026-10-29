use nom::Parser;
use nom::bytes::tag;
use nom::bytes::take;
use nom::combinator::map;
use nom::error::ErrorKind;
use nom::number::le_u32;
use nom::sequence::preceded;
use std::{fs, io};
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::{BinaryRepresentation, ObjectStore, Error};
use crate::crc::check_crc;
use crate::page::Page;

pub(crate) trait BlockRead: io::Read {
    fn block_read(&mut self, len: usize) -> crate::Result<Box<[u8]>> {
        let mut buf = vec![0u8; len].into_boxed_slice();
        io::Read::read_exact(self, &mut buf)?;
        Ok(buf)
    }
}

pub(crate) trait BlockWrite: io::Write {
    fn block_write(&mut self, buffer: impl AsRef<[u8]>) -> crate::Result<()> {
        io::Write::write_all(self, buffer.as_ref())?;
        Ok(())
    }
}

pub(crate) trait BlockSeek: io::Seek {
    fn block_seek(&mut self, by: isize) -> crate::Result<()> {
        io::Seek::seek_relative(self, by as i64)?;
        Ok(())
    }
}

 impl<Block: io::Read> BlockRead for Block {}
 impl<Block: io::Write> BlockWrite for Block {}
 impl<Block: io::Seek> BlockSeek for Block {}


pub struct BlockDriver<Backing> {
    path: PathBuf,
    backing: Backing,

    uuid: Uuid,
    version: u32,
    page_size: u32,
    page_offset: u32,
}

impl BlockDriver<fs::File> {
    pub const PAGE_SIZE: usize = 16_777_216;
    pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(true)
            .create(true)
            .open(path.as_ref())?;

        let input = file.block_read(36)?;
        let superblock = try {
            let (input, uuid) =
                map(preceded(tag(&b"fsdb"[..]), take(16usize)), Uuid::from_slice)
                    .parse(&input)?;

            let uuid =
                uuid.map_err(|err| nom::Err::Error(nom::error::Error::new(input, ErrorKind::IsNot)))?;

            let (_, (version, block_size, block_offset, crc)) =
                (le_u32(), le_u32(), le_u32(), le_u32())
                    .parse_complete(input)?;

            (crc, BlockDriver {
                path: path.as_ref().to_path_buf(),
                backing: file,

                uuid,
                version,
                page_size: block_size,
                page_offset: block_offset
            })
        };

        match superblock {
            Ok((crc, driver)) if check_crc(&input[0..32], crc) => Ok(driver),
            Ok(_) => Err(crate::Error::new("Superblock corrupt.")),
            Err(err) => Err(crate::Error::new(err.to_string()))
        }
    }

    pub fn blank(path: impl AsRef<Path>) -> crate::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(true)
            .create(true)
            .open(path.as_ref())?;

        let mut driver = BlockDriver {
            path: path.as_ref().to_owned(),
            backing: file,
            uuid: Uuid::new_v4(),
            version: 0,
            page_size: BlockDriver::PAGE_SIZE as u32,
            page_offset: 0x0000_0030u32,
        };

        let mut vec = Vec::new();
        vec.extend_from_slice(&b"fsdb"[..]);
        vec.extend_from_slice(driver.uuid.as_bytes());
        vec.extend_from_slice(&driver.version.to_le_bytes()[..]);
        vec.extend_from_slice(&driver.page_size.to_le_bytes()[..]);
        vec.extend_from_slice(&driver.page_offset.to_le_bytes()[..]);
        vec.extend_from_slice(&crate::crc(&vec)[..]);

        vec.resize(BlockDriver::PAGE_SIZE + driver.page_offset as usize, 0u8);

        let mut cursor = io::Cursor::new(&mut vec[driver.page_offset as usize..]);

        Page::new(0, BlockDriver::PAGE_SIZE).write_struct(&mut cursor)?;

        driver.backing.block_write(vec)?;

        Ok(driver)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<BlockDev> ObjectStore for BlockDriver<BlockDev> {
    fn index_db(&self) -> crate::Result<(), Error> {
        todo!()
    }

    fn compact_db(&mut self) -> crate::Result<()> {
        todo!()
    }

    fn append_object(&mut self, obj: &[u8]) -> crate::Result<Uuid> {
        todo!()
    }

    fn search_object(&mut self, obj: Uuid) -> Option<Box<[u8]>> {
        todo!()
    }
}