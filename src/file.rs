use crate::{BinaryRepresentation, Error};
use crate::ObjectStore;
use crate::Result;
use crate::crc::{check_crc, Crc};
use crate::io_utils::{Device, Reader, Writer};
use crate::object::{Object, ObjectHeader, ObjectKind};
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
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

pub struct BlockDriver<Backing: Device> {
    // path: PathBuf,
    backing: Writer<Backing>,

    uuid: Uuid,
    version: u32,
    page_size: u32,
    page_offset: u32,

    index: Option<Index>,
    active_page: Option<Page>,
}

impl<BlockDev: Device> BlockDriver<BlockDev> {
    pub const PAGE_SIZE: usize = 16_777_216;

    fn open(dev: BlockDev) -> Result<Self> {
        let input = dev.read_at(0, 36)?;
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
                    backing: Writer::new(dev),

                    uuid,
                    version,
                    page_size: block_size,
                    page_offset: block_offset,

                    active_page: None,

                    index: None,
                },
            )
        };

        match superblock {
            Ok((crc, mut driver)) if check_crc(&input[0..32], crc) => {
                BlockDriver::index(&mut driver)?;
                Ok(driver)
            }
            Ok(_) => Err(crate::Error::new("Superblock corrupt.")),
            Err(err) => Err(crate::Error::new(err.to_string())),
        }
    }

    fn blank(dev: BlockDev) -> Result<Self> {
        let mut driver = BlockDriver {
            backing: Writer::new(dev),
            uuid: Uuid::new_v4(),
            version: 0,
            page_size: Self::PAGE_SIZE as u32,
            page_offset: Page::FIRST_PAGE_OFFSET as u32,

            active_page: None,

            index: Some(Index::with_hasher(BuildHasherDefault::new())),
        };

        let mut vec = Vec::new();
        vec.extend_from_slice(&b"fsdb"[..]);
        vec.extend_from_slice(driver.uuid.as_bytes());
        vec.extend_from_slice(&driver.version.to_le_bytes()[..]);
        vec.extend_from_slice(&driver.page_size.to_le_bytes()[..]);
        vec.extend_from_slice(&driver.page_offset.to_le_bytes()[..]);
        vec.extend_from_slice(&crate::crc(&vec)[..]);

        driver.backing.append(&vec)?;
        driver.backing.advance_to_alignment(driver.page_offset as u64);

        let page = Page::new(0, Self::PAGE_SIZE);
        page.write_struct(&mut driver.backing)?;
        driver.active_page.replace(page);

        Ok(driver)
    }

    fn load_page(&self, page_number: PageNr) -> Result<Page> {
        // TODO: reuse parsed headers; they're not gonna change
        Ok(Page::read_struct(self.backing.reader(), self.page_offset as u64 + page_number as u64 * self.page_size as u64)?)
    }

    fn index(&mut self) -> Result<()> {
        let mut records = 0usize;
        for page_nr in 0 as PageNr.. {
            let mut page = self.load_page(page_nr)?;

            records += page.count_records(self.backing.reader());

            if !page.is_sealed() {
                page.preload_records(self.backing.reader())?;
                self.active_page.replace(page);
                break;
            }
        }

        let mut index = Index::with_capacity_and_hasher(records, BuildHasherDefault::new());

        for page_nr in 0..records as PageNr {
            let page = self.load_page(page_nr)?;

            for record in page.iter(self.backing.reader()) {
                let Ok(record) = record else {
                    continue;
                };

                if record.kind == ObjectKind::Tombstone {
                    continue;
                }

                index
                    .entry(record.uuid.as_u128())
                    .and_modify(|entry| entry.insert(page_nr as PageNr, record.offset))
                    .or_insert(Location::new(page_nr as PageNr, record.offset));
            }
        }

        self.index.replace(index);

        Ok(())
    }

    fn active_page_mut(&mut self) -> &mut Page {
        if let Some(ref mut page) = self.active_page && !page.is_sealed() {
            return page;
        }

        if let Some(page_nr) = self.index.as_ref()
            .and_then(|index| index.values().map(|i| i.latest().0).max()) &&
            let Ok(page) = self.load_page(page_nr) {

            if !page.is_sealed() {
                self.active_page.replace(page);
            }
        }

        if let Some(ref mut page) = self.active_page && !page.is_sealed() {
            return page;
        }

        self.index().expect("Failed to index database");

        if let Some(ref mut page) = self.active_page && !page.is_sealed() {
            return page;
        }

        panic!("Retrieving the active page failed. Something's truly broken.");
    }

    fn new_page(&mut self) -> Result<()> {
        todo!()
    }
}

pub fn open(path: impl AsRef<Path>) -> Result<BlockDriver<fs::File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path.as_ref())?;

    BlockDriver::open(file)
}

pub fn blank(path: impl AsRef<Path>) -> Result<BlockDriver<fs::File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .create(true)
        .open(path.as_ref())?;

    BlockDriver::blank(file)
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
        self.0[self.0.len() - 1]
    }

    fn new(page: PageNr, offset: Offset) -> Self {
        Self(vec![(page, offset)])
    }

    fn insert(&mut self, page: PageNr, offset: Offset) {
        self.0.push((page, offset));
    }
}

type Index = HashMap<u128, Location, BuildHasherDefault<FoldHasher>>;

impl<BlockDev: Device> ObjectStore for BlockDriver<BlockDev> {
    fn compact_db(&mut self) -> Result<()> {
        todo!()
    }

    fn entries(&self) -> ObjectIterator<'_, Self> { ObjectIterator(self) }

    fn search(&self, id: Uuid) -> Result<Option<Object>> {
        let Some(ref index) = self.index else {
            return Err(Error::new("Database is not indexed properly. Please re-open the database"));
        };

        let Some(location) = index.get(&id.as_u128()) else {
            return Ok(None);
        };

        let page = self.load_page(location.latest().0)?;
        page.get_object_at(location.latest().1 as u32, self.backing.reader())
    }

    fn insert(&mut self, data: &[u8]) -> Result<Uuid> {
        // 1. Check whether the current page has enough space on it
        // 2. Allocate a new page if necessary and write it into `self`
        // 3. Take the latest page and write the object there.
        // 4. Update the page to include the object in the index

        if self.active_page_mut().can_fit_object_of_length(data.len()) {
            self.new_page()?;
        }

        let mut crc = Crc::new();
        let uuid = Uuid::new_v4();

        crc.update(uuid);
        crc.update((data.len() as u32).to_le_bytes());

        // We've called `.active_page_mut()` enough to be reasonably sure that it's set
        let offset = self.active_page.as_ref()
            .and_then(|page| page.next_free(self.backing.reader()))
            .unwrap_or(Page::PAGE_HEADER_SIZE as Offset);

        let obj = ObjectHeader {
            uuid,
            length: data.len(),
            kind: ObjectKind::Structured,
            crc: u32::from_le_bytes(crc.finish()),
            offset,
        };

        let seek = self.active_page_mut().page_offset + offset as u64;
        self.backing.try_seek_to(seek)?;
        obj.write_struct(&mut self.backing)?;
        self.backing.append(data)?;

        self.active_page_mut()
            .contents
            .get_or_insert(vec![])
            .push(obj);

        self.backing.sync()?;

        let page = self.active_page_mut().seq;
        if let Err(err) = self.index
            .get_or_insert_with(|| Default::default())
            .try_insert(uuid.as_u128(), Location::new(page, offset)) {
            return Err(Error::new("Object already inserted."));
        };

        Ok(uuid)
    }

    fn update(&mut self, id: Uuid, data: &[u8]) -> Result<()> {
        todo!()
    }

    fn delete(&mut self, id: Uuid) -> Result<()> {
        todo!()
    }
}

pub struct ObjectIterator<'a, Store: ObjectStore>(&'a Store);

impl<'store, Store: ObjectStore> Iterator for ObjectIterator<'store, Store> {
    type Item = Object;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}