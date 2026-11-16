use crate::Error;
use crate::ObjectStore;
use crate::Result;
use crate::object::Object;
use crate::object::ObjectHeader;
use crate::object::ObjectKind;
use crate::page::{Page, PageHeader};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fs;
use std::fs::OpenOptions;
use std::hash::BuildHasherDefault;
use std::hash::Hasher;
use std::path::Path;
use std::sync::Arc;
use std::sync::nonpoison::RwLock;
use util::Device;
use util::Writer;
use uuid::Uuid;
use format::binary;
use util::binary::{Binary, Fixed};

pub(crate) enum LoadedPage {
    Sealed(Arc<Page>),
    Open(Page)
}

impl std::ops::Deref for LoadedPage {
    type Target = Page;

    fn deref(&self) -> &Self::Target {
        match self {
            LoadedPage::Sealed(page) => page.as_ref(),
            LoadedPage::Open(page) => page
        }
    }
}

pub struct BlockDriver<Backing: Device> {
    // path: PathBuf,
    pub(crate) backing: Writer<Backing>,

    uuid: Uuid,
    version: u32,
    page_size: u32,
    page_offset: u32,

    index: Option<Index>,
    active_page: Option<Page>,

    // TODO: purge LRU-style old data.
    page_headers_cache: RwLock<HashMap<PageNr, Arc<Page>>>
}

impl<BlockDev: Device> BlockDriver<BlockDev> {
    pub const PAGE_SIZE: usize = 16_777_216;

    fn open(dev: BlockDev) -> Result<Self> {
        let dev = Writer::new(dev);
        let superblock = Superblock::read(&mut dev.reader())?;

        let mut driver = Self {
            backing: dev,
            uuid: superblock.uuid,
            version: superblock.version,
            page_size: superblock.page_size,
            page_offset: superblock.page_offset,
            index: None,
            active_page: None,
            page_headers_cache: RwLock::new(HashMap::new())
        };

        driver.index()?;

        Ok(driver)
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
            page_headers_cache: RwLock::new(HashMap::new())
        };

        let superblock = Superblock {
            uuid: driver.uuid,
            version: driver.version,
            page_size: driver.page_size,
            page_offset: driver.page_offset,
        };

        superblock.write(&mut driver.backing)?;
        driver.new_page()?;

        Ok(driver)
    }

    pub(crate) fn load_page(&self, page_nr: PageNr) -> Result<LoadedPage> {
        if let Some(page) = self.page_headers_cache.read().get(&page_nr) {
            return Ok(LoadedPage::Sealed(page.clone()));
        }

        let mut page = self.load_page_from_disk(page_nr)?;

        if page.is_sealed() {
            let page = Arc::new(page);
            self.page_headers_cache.write().insert(page_nr, page.clone());

            return Ok(LoadedPage::Sealed(page.clone()))
        }

        page.preload_records(self.backing.reader())?;
        Ok(LoadedPage::Open(page))
    }

    fn load_page_from_disk(&self, page_nr: PageNr) -> Result<Page> {
        let mut reader = self.backing.reader();
        reader.seek_to(self.page_offset as u64 + page_nr as u64 * self.page_size as u64);
        Ok(Page::read_struct(&mut reader)?)
    }

    fn index(&mut self) -> Result<()> {
        let mut records = 0usize;
        let page_count = self.page_count()? as PageNr;

        for page_nr in 0..page_count {
            records += self.load_page(page_nr)?.count_records(self.backing.reader());
        }

        let mut index = Index::with_capacity_and_hasher(records, BuildHasherDefault::new());

        for page_nr in 0..page_count {
            let page = self.load_page(page_nr)?;

            for record in page.iter(self.backing.reader()) {
                let Ok(record) = record else {
                    continue;
                };

                if record.kind == ObjectKind::Tombstone {
                    index.remove(&record.uuid.as_u128());
                    continue;
                }

                let page_offset = record.position - page.page_start;

                index
                    .entry(record.uuid.as_u128())
                    .and_modify(|entry| entry.insert(page_nr as PageNr, page_offset as Offset))
                    .or_insert(Location::new(page_nr as PageNr, page_offset as Offset));
            }
        }

        self.index.replace(index);

        Ok(())
    }

    fn get_record(&self, record: &Location) -> Result<Option<Object>> {
        let record = record.latest();
        let page = self.load_page(record.0)?;
        Ok(page.get_object_at(record.1, self.backing.reader())?)
    }

    fn get_record_header(&self, record: &Location) -> Result<Option<ObjectHeader>> {
        let record = record.latest();
        let page = self.load_page(record.0)?;
        Ok(page.get_object_header_at(record.1, self.backing.reader())?)
    }

    fn page_count(&self) -> Result<usize> {
        let max = self.get_last_known_page_nr();

        for page_nr in max.. {
            if !self.page_exists(page_nr)? {
                return Ok(page_nr as usize)
            }
        }

        unreachable!()
    }

    fn page_exists(&self, page_nr: PageNr) -> Result<bool> {
        let start = self.page_offset as u64 + page_nr * self.page_size as u64;
        let mut reader = self.backing.reader().with_seek(start);

        match reader.read(PageHeader::SIZE) {
            Err(err) if err.is_eof() => Ok(false),
            Err(err) => Err(err),
            Ok(raw) if raw.iter().all(|b| *b == 0) => Ok(false),
            Ok(_) => {
                // Something is there: it must be a valid header.
                PageHeader::read(&mut self.backing.reader().with_seek(start))?;
                Ok(true)
            }
        }
    }

    fn get_last_known_page_nr(&self) -> PageNr {
        self.index
            .as_ref()
            .and_then(|index| index.values().map(|i| i.latest().0).max())
            .unwrap_or(0)
    }

    fn active_page_mut(&mut self) -> Result<&mut Page> {
        if let None = self.active_page {
            let count = self.page_count()? as PageNr;

            return if count > 0 && let LoadedPage::Open(page) = self.load_page(count - 1)? {
                self.active_page.replace(page);
                Ok(self.active_page.as_mut().expect("Set above"))
            } else {
                Ok(self.new_page()?)
            }
        }

        let have_active = self.active_page.as_ref()
            .is_some_and(|p| !p.is_sealed());

        if !have_active {
            let start = self.get_last_known_page_nr();

            for page_nr in start..self.page_count()? as PageNr {
                match self.load_page(page_nr)? {
                    LoadedPage::Sealed(_) => continue,
                    LoadedPage::Open(page) => {
                        self.active_page.replace(page);
                        return Ok(self.active_page.as_mut().expect("Set above"));
                    }
                }
            }

            return Ok(self.new_page()?)
        }

        return Ok(self.active_page.as_mut().expect("Set above"))
    }

    fn new_page(&mut self) -> Result<&mut Page> {
        let page_nr = if let Some(ref mut page) = self.active_page {
            page.seal(&mut self.backing)?;
            page.seq + 1
        } else { self.page_count()? as u64 };

        let mut page = Page::new(page_nr, self.page_size as usize, self.page_offset as u64);
        page.contents.replace(Default::default());
        page.write_struct(&mut self.backing)?;

        self.active_page.replace(page);

        // TODO: dump the content index for the previous page

        Ok(self.active_page.as_mut().expect("Infallible"))
    }

    pub(crate) fn get_location(&self, uuid: Uuid) -> Option<&Location> {
        self.index.as_ref()
            .and_then(|index| index.get(&uuid.as_u128()))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item=ObjectHeader> {
        gen {
            if let Some(ref index) = self.index {
                for record in index.values() {
                    let Ok(Some(record)) = self.get_record_header(record) else {
                        continue;
                    };

                    yield record;
                }
            }
        }
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
pub(crate) struct FoldHasher(u64);

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

pub(crate) struct Location(Vec<(PageNr, Offset)>);

impl Location {
    pub(crate) fn latest(&self) -> (PageNr, Offset) {
        self.0[self.0.len() - 1]
    }

    fn new(page: PageNr, offset: Offset) -> Self {
        Self(vec![(page, offset)])
    }

    fn insert(&mut self, page: PageNr, offset: Offset) {
        self.0.push((page, offset));
    }
}

pub(crate) type Index = HashMap<u128, Location, BuildHasherDefault<FoldHasher>>;

impl<BlockDev: Device> ObjectStore for BlockDriver<BlockDev> {
    fn id(&self) -> Uuid {
        self.uuid
    }

    fn compact_db(&mut self) -> Result<()> {
        todo!()
    }

    fn entries(&self) -> impl Iterator<Item = Object> { ObjectIterator(self) }

    fn fetch(&self, id: Uuid) -> Result<Option<Object>> {
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
        let uuid = Uuid::new_v4();

        self.upsert(uuid, data)?;

        Ok(uuid)
    }

    fn upsert(&mut self, uuid: Uuid, data: &[u8]) -> Result<()> {
        if data.len() as u64 > Page::MAX_OBJECT_PAYLOAD {
            return Err(Error::new("Object payload is too large."));
        }

        // 1. Check whether the current page has enough space on it
        // 2. Allocate a new page if necessary and write it into `self`
        // 3. Take the latest page and write the object there.
        // 4. Update the page to include the object in the index

        if !self.active_page_mut()?.can_fit_object_of_length(data.len()) {
            self.new_page()?;
        }

        let mut crc = util::Crc::new();

        crc.update(uuid);
        crc.update((data.len() as u32).to_le_bytes());
        crc.update(data);

        // We've called `.active_page_mut()` enough to be reasonably sure that it's set
        let page_offset = self.active_page.as_ref()
            .and_then(|page| page.next_free(self.backing.reader()))
            .unwrap_or(PageHeader::SIZE as Offset)
            .next_multiple_of(Page::ALIGNMENT as u32);

        let seek = self.active_page_mut()?.page_start + page_offset as u64;

        let obj = ObjectHeader {
            uuid,
            length: data.len(),
            kind: ObjectKind::Structured,
            data_crc: crc.into_u32(),
            position: seek,
        };

        self.backing.try_seek_to(seek)?;
        obj.write(&mut self.backing)?;
        self.backing.append(data)?;

        self.active_page_mut()?
            .contents
            .get_or_insert(vec![])
            .push(obj);

        self.backing.sync()?;

        let page = self.active_page_mut()?.seq;
        self.index
            .get_or_insert_with(|| Default::default())
            .insert(uuid.as_u128(), Location::new(page, page_offset));

        Ok(())
    }

    fn delete(&mut self, uuid: Uuid) -> Result<()> {
        if !self.active_page_mut()?.can_fit_object_of_length(0) {
            self.new_page()?;
        }

        let mut crc = util::Crc::new();

        crc.update(uuid);
        crc.update([0u8; 4]);

        // We've called `.active_page_mut()` enough to be reasonably sure that it's set
        let page_offset = self.active_page.as_ref()
            .and_then(|page| page.next_free(self.backing.reader()))
            .unwrap_or(PageHeader::SIZE as Offset)
            .next_multiple_of(Page::ALIGNMENT as u32);

        let seek = self.active_page_mut()?.page_start + page_offset as u64;

        let obj = ObjectHeader {
            uuid,
            length: 0,
            kind: ObjectKind::Tombstone,
            data_crc: crc.into_u32(),
            position: seek,
        };

        self.backing.try_seek_to(seek)?;
        obj.write(&mut self.backing)?;

        self.active_page_mut()?
            .contents
            .get_or_insert(vec![])
            .push(obj);

        self.backing.sync()?;

        self.index.get_or_insert_with(|| Default::default())
            .remove(&uuid.as_u128());

        Ok(())
    }
}

pub struct ObjectIterator<'a, Store: ObjectStore>(&'a Store);

impl<'store, Store: ObjectStore> Iterator for ObjectIterator<'store, Store> {
    type Item = Object;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

#[binary(magic(b"fsdb"), Uuid(uuid), u32(version), u32(page_size), u32(page_offset), crc(magic, uuid, version, page_size, page_offset))]
struct Superblock {
    #[from(uuid)]
    uuid: Uuid,

    #[from(version)]
    version: u32,

    #[from(page_size)]
    page_size: u32,

    #[from(page_offset)]
    page_offset: Offset
}