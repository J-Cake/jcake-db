use crate::Error;
use crate::Result;
use crate::file::Offset;
use crate::object::{Object, ObjectKind};
use crate::object::ObjectHeader;
use std::collections::HashMap;
use std::iter::Filter;
use util::Device;
use util::Reader;
use util::Writer;
use uuid::Uuid;
use format::binary;
use util::binary::{Binary, Fixed};

pub(crate) struct Page {
	pub(crate) seq: u64,
	pub(crate) page_start: u64,
	page_size: usize,

	pub(crate) contents: Option<Vec<ObjectHeader>>,

	trailer: Option<PageTrailer>,
	created: Timestamp,
}

type Timestamp = chrono::DateTime<chrono::Utc>;

#[binary(magic(b"page"), u64(seq), u32(len), Timestamp(created), crc(magic, seq, len, created), ptr(pos))]
pub(crate) struct PageHeader {
	#[from(seq)]
	seq: u64,

	#[from(pos)]
	page_start: u64,

	#[from(len)]
	page_size: u32,

	#[from(created)]
	created: Timestamp
}

impl PageHeader {
	pub(crate) fn from_page(page: &Page) -> Self {
		Self {
			seq: page.seq,
			page_start: page.page_start,
			page_size: page.page_size as u32,
			created: page.created,
		}
	}
}

const SUMMARY_MAGIC_NUMBER_LENGTH: usize = 4;

#[binary(magic(b"smry"), vec(summary))]
struct PageSummary {
	#[from(summary)]
	summary: Vec<PageSummaryEntry>
}

#[binary(Uuid(uuid), u32(offset))]
struct PageSummaryEntry {
	#[from(uuid)]
	uuid: Uuid,

	#[from(offset)]
	offset: Offset
}

#[binary(magic(b"foot"), u32(len), crc(magic, len))]
struct PageFooter {
	#[from(len)]
	len: usize,
}

struct PageTrailer {
	global_offset: u64,
	summary: Option<HashMap<Uuid, u32>>,
	num_items: usize
}

impl Page {
	// pub(crate) const FIRST_PAGE_OFFSET: u64 = 0x1_0000;
	pub(crate) const FIRST_PAGE_OFFSET: u64 = 0x50;
	pub(crate) const ALIGNMENT: u64 = 0x10;
	pub(crate) const MAX_OBJECT_PAYLOAD: u64 = 0x400_000;

	pub(crate) fn new(seq: u64, size: usize) -> Page {
		Self {
			seq,
			page_size: size,
			page_start: Self::FIRST_PAGE_OFFSET + seq * size as u64,
			created: Default::default(),

			contents: Default::default(),
			trailer: None,
		}
	}

	pub(crate) fn read_struct(dev: &mut Reader<impl Device>) -> Result<Self> {
		let offset = dev.position();

		let header = PageHeader::read(dev)?;
		dev.seek_to(offset + (header.page_size as u64 - PageFooter::SIZE as u64));

		let summary = if let Ok(footer) = PageFooter::read(dev) && footer.len > 0 {
			let summary_start = header.page_size as u64 - PageFooter::SIZE as u64 - footer.len as u64 * PageSummaryEntry::SIZE as u64 - SUMMARY_MAGIC_NUMBER_LENGTH as u64;
			dev.seek_to(offset + summary_start);

			let summary = PageSummary::read(dev)?;

			Some(PageTrailer {
				global_offset: offset + summary_start,
				summary: Some(summary.summary.into_iter()
					.map(|entry| (entry.uuid, entry.offset))
					.collect()),
				num_items: footer.len,
			})
		} else {
			None
		};

		let mut page = Self {
			seq: header.seq,
			page_start: offset,
			page_size: header.page_size as usize,
			contents: None,
			trailer: summary,
			created: header.created,
		};

		if !page.is_sealed() {
			page.preload_records(dev.clone())?;
		}

		Ok(page)
	}

	pub(crate) fn write_struct(&self, dev: &mut Writer<impl Device>) -> Result<()> {
		dev.try_seek_to(self.page_start)?;

		if !self.is_sealed() {
			PageHeader {
				seq: self.seq,
				page_start: self.page_start,
				page_size: self.page_size as u32,
				created: self.created,
			}.write(dev)?;
		} else {
			return Err(Error::recoverable("Attempted to write a sealed a page"))
		}

		Ok(())
	}

	pub(crate) fn is_sealed(&self) -> bool {
		self.trailer.is_some()
	}

	pub(crate) fn count_records(&self, dev: Reader<impl Device>) -> usize {
		if let Some(summary) = self.trailer.as_ref()
			.and_then(|trailer| trailer.summary.as_ref()) {
			return summary.len();
		} else if let Some(ref contents) = self.contents {
			return contents.len();
		}

		self.iter(dev.clone())
			.count()
	}

	pub(crate) fn preload_records(&mut self, dev: Reader<impl Device>) -> Result<()> {
		self.contents.replace(self.iter(dev).collect::<Result<_>>()?);
		Ok(())
	}

	pub(crate) fn iter<'a, Dev: Device>(&'a self, mut dev: Reader<'a, Dev>) -> PageIter<'a, Dev> {
		dev.seek_to(self.page_start);
		PageIter {
			page: self,
			backing: dev,
			did_return_error: false,
			advance: match self.contents {
				Some(ref cache) => IterAdvance::FromCache {
					cache,
					index: 0
				},
				None => IterAdvance::FromDisk {
					offset: PageHeader::SIZE.next_multiple_of(Page::ALIGNMENT as usize) as Offset
				}
			}
		}
	}

	/// Get the offset of the position to insert the next object **relative to the start of the page**
	pub(crate) fn next_free(&self, dev: Reader<impl Device>) -> Option<Offset> {
		if let Some(last) = self.contents.as_ref().and_then(|content| content.last()) {
			let page_offset = (last.position - self.page_start) as Offset;
			return Some((page_offset + last.length as Offset + ObjectHeader::SIZE as Offset)
				.next_multiple_of(Page::ALIGNMENT as u32));
		}

		self.iter(dev)
			.try_fold(None, |_, r| r.map(Some))
			.ok()?
			.map(|i| (i.position - self.page_start) as Offset + i.length as Offset + ObjectHeader::SIZE as Offset)
			.map(|i| i.next_multiple_of(Page::ALIGNMENT as Offset))
	}

	pub(crate) fn get_object_at(&self, offset: Offset, mut dev: Reader<impl Device>) -> Result<Option<Object>> {
		dev.seek_to(self.page_start + offset as u64);
		let header = ObjectHeader::read(&mut dev)?;

		Object::from_header(header, &mut dev)
	}

	pub(crate) fn get_object_header_at(&self, offset: Offset, mut dev: Reader<impl Device>) -> Result<Option<ObjectHeader>> {
		dev.seek_to(self.page_start + offset as u64);
		Ok(Some(ObjectHeader::read(&mut dev)?))
	}

	pub(crate) fn can_fit_object_of_length(&self, len: usize) -> bool {
		if len as u64 > Self::MAX_OBJECT_PAYLOAD {
			return false;
		}

		let Some(c) = self.contents.as_ref() else {
			// If the `contents` is empty, we can be fairly certain the page is sealed.
			// Returning `false` on a false negative is fine since we'd rather be safe than writing to an over-full page.
			// We can optionally check whether we're _actually_ sealed, and on a mismatch, we could try to correct the issue, but that's not trivial to do.

			return false;
		};

		let offset = c.last()
			.map(|i| (i.position - self.page_start) as Offset + i.length as Offset + ObjectHeader::SIZE as Offset)
			.unwrap_or(PageHeader::SIZE as Offset)
			.next_multiple_of(Page::ALIGNMENT as u32);

		let theoretical_summary_offset = self.page_size - PageFooter::SIZE - c.len() * PageSummaryEntry::SIZE - SUMMARY_MAGIC_NUMBER_LENGTH;

		(offset + len as Offset + ObjectHeader::SIZE as Offset) < theoretical_summary_offset as Offset
	}

	pub(crate) fn seal(&mut self, writer: &mut Writer<impl Device>) -> Result<()> {
		if let Some(ref c) = self.contents {
			// Check if we can still fit a summary. Skip if not.

			let theoretical_summary_offset = (self.page_size - PageFooter::SIZE - c.len() * PageSummaryEntry::SIZE - SUMMARY_MAGIC_NUMBER_LENGTH) as u64 + self.page_start;

			// If we've already written past where the summary should go, then skip it.
			// A situation could occur where an object is written into the space the summary should go, then the database was closed and reopened, such that the cursor hasn't caught up yet.
			if writer.position() <= theoretical_summary_offset && c.last()
				.is_some_and(|obj| obj.position + obj.length as u64 + ObjectHeader::SIZE as u64 <= theoretical_summary_offset) {

				writer.try_seek_to(theoretical_summary_offset)?;

				PageSummary {
					summary: c.iter()
						.map(|i| PageSummaryEntry {
							uuid: i.uuid,
							offset: (i.position - self.page_start) as Offset,
						})
						.collect(),
				}.write(writer)?;

				PageFooter {
					len: c.len()
				}.write(writer)?;
			}
		};

		Ok(())
	}
}

pub(crate) struct PageIter<'a, Dev: Device> {
	page: &'a Page,
	backing: Reader<'a, Dev>,
	did_return_error: bool,
	advance: IterAdvance<'a>
}

enum IterAdvance<'a> {
	FromCache {
		cache: &'a [ObjectHeader],
		index: usize
	},
	FromDisk {
		offset: Offset
	}
}

impl<'a, Dev: Device> Iterator for PageIter<'a, Dev> {
	type Item = Result<ObjectHeader>;

	fn next(&mut self) -> Option<Self::Item> {
		if self.did_return_error {
			return None;
		}

		match self.advance {
			IterAdvance::FromCache { cache, ref mut index } => {
				if *index < cache.len() {
					*index += 1;
					Some(Ok(cache[*index - 1]))
				} else {
					None
				}
			}
			IterAdvance::FromDisk { ref mut offset } => {
				self.backing.seek_to(self.page.page_start + *offset as u64);
				match ObjectHeader::read(&mut self.backing) {
					Ok(header) => {
						*offset += (ObjectHeader::SIZE as Offset + header.len() as Offset)
							.next_multiple_of(Page::ALIGNMENT as u32);
						if *offset > self.page.page_size as Offset - PageFooter::SIZE as Offset {
							return None;
						}
						Some(Ok(header))
					}
					Err(e) if e.is_recoverable() => None,
					Err(e) => {
						self.did_return_error = true;
						Some(Err(e))
					},
				}
			}
		}
	}
}