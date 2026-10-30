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
use std::hash::{BuildHasher, BuildHasherDefault};
use std::hash::Hasher;
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

    index: Option<Index>
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

                    index: None
                },
            )
        };

        match superblock {
            Ok((crc, mut driver)) if check_crc(&input[0..32], crc) => {
                driver.index.replace(driver.index()?);

                Ok(driver)
            },
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
            page_offset: Page::PAGE_OFFSET as u32,

            index: Some(Index::with_hasher(BuildHasherDefault::new()))
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
}

impl<BlockDev: Device> BlockDriver<StickyFile<BlockDev>> {
    fn load_page(&self, page_number: usize) -> Result<Page> {
        let offset = self.page_offset as i64 + page_number as i64 * self.page_size as i64;
        self.backing.seek_to(offset);

        Ok(Page::read_struct(&self.backing)?)
    }

    fn index(&self) -> Result<Index> {
        let mut records = 0usize;
        for page in 0usize.. {
            let page = self.load_page(page)?;

            records += page.count_records(&self.backing);

            if !page.is_sealed() {
                break;
            }
        }

        let mut index = Index::with_capacity_and_hasher(records, BuildHasherDefault::new());

        for page_nr in 0..records {
            let page = self.load_page(page_nr)?;

            for record in page.iter(&self.backing) {
                let Ok(record) = record else {
                    continue;
                };

                index.entry(record.uuid.as_u128())
                    .and_modify(|entry| entry.insert(page_nr as PageNr, record.offset))
                    .or_insert(Location::new(page_nr as PageNr, record.offset));
            }
        }

        Ok(index)
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

pub(crate) type PageNr = u64;
pub(crate) type Offset = u32;
struct Location(Vec<(PageNr, Offset)>);

impl Location {
    fn latest(&self) -> (PageNr, Offset) {
        self.0[0]
    }

    fn new(page: PageNr, offset: Offset) -> Self {
        Self(vec![(page, offset)])
    }

    fn insert(&mut self, page: PageNr, offset: Offset) {
        self.0.push((page, offset));
    }
}

type Index = HashMap<u128, Location, BuildHasherDefault<FoldHasher>>;

impl<BlockDev: Device> ObjectStore for BlockDriver<StickyFile<BlockDev>> {
    fn compact_db(&mut self) -> Result<()> {
        todo!()
    }

    fn search(&self, id: Uuid) -> Result<Option<Object>> {
        let Some(ref index) = self.index else {
            return Err(crate::Error::new("Database is not indexed properly. Please re-open the database"));
        };

        let Some(location) = index.get(&id.as_u128()) else {
            return Ok(None);
        };

        let page = self.load_page(location.latest().0 as usize)?;
        page.get_object_at(location.latest().1 as u32, &self.backing)
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
