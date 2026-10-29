use crate::BinaryRepresentation;
use crate::ObjectStore;
use crate::Result;
use crate::crc::check_crc;
use crate::page::Page;
use nom::Parser;
use nom::bytes::tag;
use nom::bytes::take;
use nom::combinator::map;
use nom::error::ErrorKind;
use nom::number::le_u32;
use nom::sequence::preceded;
use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::hash::BuildHasherDefault;
use std::hash::Hasher;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;
use crate::io_utils::{Device, StickyFile};
use crate::object::Object;

pub struct BlockDriver<Backing> {
    path: PathBuf,
    backing: Backing,

    uuid: Uuid,
    version: u32,
    page_size: u32,
    page_offset: u32,
}

impl BlockDriver<StickyFile<fs::File>> {
    pub const PAGE_SIZE: usize = 16_777_216;

    pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
        let file = StickyFile::new(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.as_ref())?);

        let input = file.read_at(0, 36)?;
        let superblock = try {
            let (input, uuid) =
                map(preceded(tag(&b"fsdb"[..]), take(16usize)), Uuid::from_slice).parse(&input)?;

            let uuid = uuid
                .map_err(|err| nom::Err::Error(nom::error::Error::new(input, ErrorKind::IsNot)))?;

            let (_, (version, block_size, block_offset, crc)) =
                (le_u32(), le_u32(), le_u32(), le_u32()).parse_complete(input)?;

            (
                crc,
                BlockDriver {
                    path: path.as_ref().to_path_buf(),
                    backing: file,

                    uuid,
                    version,
                    page_size: block_size,
                    page_offset: block_offset,
                },
            )
        };

        match superblock {
            Ok((crc, driver)) if check_crc(&input[0..32], crc) => Ok(driver),
            Ok(_) => Err(crate::Error::new("Superblock corrupt.")),
            Err(err) => Err(crate::Error::new(err.to_string())),
        }
    }

    pub fn blank(path: impl AsRef<Path>) -> crate::Result<Self> {
        let file = StickyFile::new(OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(true)
            .create(true)
            .open(path.as_ref())?);

        let mut driver = BlockDriver {
            path: path.as_ref().to_owned(),
            backing: file,
            uuid: Uuid::new_v4(),
            version: 0,
            page_size: BlockDriver::PAGE_SIZE as u32,
            page_offset: 0x0001_0000u32,
        };

        let mut vec = Vec::new();
        vec.extend_from_slice(&b"fsdb"[..]);
        vec.extend_from_slice(driver.uuid.as_bytes());
        vec.extend_from_slice(&driver.version.to_le_bytes()[..]);
        vec.extend_from_slice(&driver.page_size.to_le_bytes()[..]);
        vec.extend_from_slice(&driver.page_offset.to_le_bytes()[..]);
        vec.extend_from_slice(&crate::crc(&vec)[..]);

        vec.resize(BlockDriver::PAGE_SIZE + driver.page_offset as usize, 0u8);
        let mut vec = StickyFile::new(vec);
        vec.seek_to(driver.page_offset as i64);

        Page::new(0, BlockDriver::PAGE_SIZE).write_struct(&mut vec)?;

        driver.backing.write_at(0, &vec)?;

        Ok(driver)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn index(&self) -> Result<()> {
        let mut records = 0usize;
        for page in 0usize.. {
            let page = self.load_page(page)?;

            records += page.count_records(&self.backing);

            if !page.is_sealed() {
                break;
            }
        }

        let mut index = Index::with_capacity_and_hasher(records, BuildHasherDefault::new());

        todo!();

        Ok(())
    }
}

impl<BlockDev: Device> BlockDriver<StickyFile<BlockDev>> {
    fn load_page(&self, page_number: usize) -> Result<Page> {
        let offset = self.page_offset as i64 * page_number as i64 * self.page_size as i64;
        self.backing.seek_by(offset);

        Ok(Page::read_struct(&self.backing)?)
    }
}

#[derive(Default)]
struct FoldHasher(u64);

impl Hasher for FoldHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _: &[u8]) {
        unreachable!("FoldHasher only hashes u128 keys")
    }

    fn write_u128(&mut self, v: u128) {
        self.0 = (v as u64) ^ ((v >> 64) as u64);
    }
}

struct Location {
    page: usize,
    offset: usize,
}

type Index = HashMap<u128, Location, BuildHasherDefault<FoldHasher>>;

impl<BlockDev> ObjectStore for BlockDriver<BlockDev>
where
    BlockDev: Device
{
    fn compact_db(&mut self) -> Result<()> {
        todo!()
    }

    fn search(&self, id: Uuid) -> Result<Option<Object>> {
        todo!()
    }

    fn insert(&mut self, data: &[u8]) -> Result<Uuid> {
        todo!()
    }

    fn update(&mut self, id: Uuid, data: &[u8]) -> Result<()> {
        todo!()
    }

    fn delete(&mut self, id: Uuid) -> Result<()> {
        todo!()
    }
}
