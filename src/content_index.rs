use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use util::binary::Fixed;
use util::Device;
use util::traits::ToBytes;
use crate::object::{ObjectHeader, ObjectKind};
use crate::{BlockDriver, TypedDatabase};
use crate::file::Offset;
use crate::page::{Page, PageHeader};

#[derive(Default, Debug)]
pub(crate) struct ContentIndexCx {
    forward: HashMap<Bytes, Vec<Uuid>>,
    backward: HashMap<Uuid, Vec<Bytes>>
}

impl ContentIndexCx {
    pub(crate) fn add(&mut self, uuid: Uuid, mut keys: Vec<Bytes>) {
        self.del(uuid);

        keys.sort_unstable();
        keys.dedup();

        for content in &keys {
            self.forward.entry(content.clone())
                .or_default()
                .push(uuid);
        }

        self.backward.insert(uuid, keys);
    }

    pub(crate) fn del(&mut self, uuid: Uuid) {
        if let Some(content) = self.backward.remove(&uuid) {
            for bucket in content {
                if let Some(forward) = self.forward.get_mut(&bucket) {
                    forward.retain(|i| *i != uuid)
                }
            }
        }
    }

    fn apply(&mut self, entry: IndexEntry) {
        match entry {
            IndexEntry::Add { uuid, data } => self.add(uuid, data),
            IndexEntry::Remove { uuid } => self.del(uuid),
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item=(&Bytes, &Vec<Uuid>)> {
        self.forward.iter()
    }

    pub(crate) fn get(&self, key: &[u8]) -> Option<&Vec<Uuid>> {
        self.forward.get(key)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ContentIndex(Vec<IndexEntry>);

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum IndexEntry {
    Add {
        data: Vec<Box<[u8]>>,
        uuid: Uuid,
    },
    Remove {
        uuid: Uuid
    },
}

type Bytes = Box<[u8]>;

impl<Dev: Device> TypedDatabase<BlockDriver<Dev>> {
    pub(crate) fn into_indexed(mut self) -> Self {
        let mut reader = self.backing.backing.reader();

        let obj = self.backing.iter();
        let records = gen {
            let mut last_index = None;

            for record in obj {
                if record.kind != ObjectKind::Index {
                    continue;
                }

                last_index = Some(record.clone());

                reader.seek_to(record.position + ObjectHeader::SIZE as u64);
                let Ok(record) = record.into_object(&mut reader) else {
                    continue;
                };

                let Ok(index) = record.try_into_parsed::<ContentIndex>() else {
                    continue;
                };

                for entry in index.0 {
                    yield entry;
                }
            }

            let start_iterative = last_index
                .and_then(|i| self.backing.get_location(i.uuid))
                .map(|i| i.latest())
                .unwrap_or((0, (PageHeader::SIZE as Offset).next_multiple_of(Page::ALIGNMENT as Offset)));

            let mut page_nums = start_iterative.0..;

            if let Some(Ok(first)) = page_nums.next().map(|i| self.backing.load_page(i)) {
                let total_offset = start_iterative.1 as u64 + first.page_start;
                let items = first.iter(self.backing.backing.reader());

                let mut page = vec![];
                for i in items
                    .skip_while(move |i| i.as_ref().is_ok_and(|i| i.position < total_offset))
                    .filter_map(|i| i.ok()) {

                    if let Some(record) = self.index_record(i) {
                        page.push(record);
                    }
                }

                // We unfortunately need to store a page's worth of values here because you can't yield a generator while borrowing an owned value.
                for record in page {
                    yield record;
                }
            }

            for page_nr in page_nums {
                let Ok(page) = self.backing.load_page(page_nr) else {
                    break;
                };

                let mut records = vec![];
                for i in page.iter(self.backing.backing.reader())
                    .filter_map(|i| i.ok()) {

                    if let Some(record) = self.index_record(i) {
                        records.push(record);
                    }
                }

                // same borrowing point here.
                for record in records {
                    yield record;
                }
            }
        };

        let mut index = ContentIndexCx::default();
        for entry in records {
            index.apply(entry);
        }

        self.content_index = index;
        self
    }

    fn index_record(&self, record: ObjectHeader) -> Option<IndexEntry> {
        let mut reader = self.backing.backing
            .reader()
            .with_seek(record.position + ObjectHeader::SIZE as u64);

        match record.kind {
            ObjectKind::Tombstone => Some(IndexEntry::Remove { uuid: record.uuid }),
            ObjectKind::Structured => {
                let Ok(obj) = record.into_object(&mut reader) else {
                    return None;
                };

                Some(IndexEntry::Add {
                    uuid: record.uuid,
                    data: (self.mapper)(&obj)
                        .into_iter()
                        .collect(),
                })
            },
            _ => None
        }
    }
}