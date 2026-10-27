use std::fs::OpenOptions;
use std::{fs, io};
use std::io::Write;
use std::io::Read;
use std::path::{Path, PathBuf};
use crate::{Error, StorageBackend};
use crate::superblock::Superblock;

pub struct File {
    path: PathBuf,

    superblock: Superblock
}

impl File {
    pub fn initialise(path: impl AsRef<Path>) -> crate::Result<File> {
        File {
            path: path.as_ref().to_path_buf(),
            superblock: Superblock::blank()
        }.initialise()
    }

    pub fn open(path: impl AsRef<Path>) -> crate::Result<File> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.as_ref())?;

        let mut superblock = Box::new([0u8; 28]) as Box<[u8]>;
        file.read_exact(&mut superblock)?;

        let superblock = Superblock::from_bytes(&superblock)?;

        Ok(Self {
            path: path.as_ref().to_owned(),
            superblock
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl StorageBackend for File {
    fn initialise(self) -> crate::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(true)
            .create(true)
            .open(&self.path)?;

        file.write_all(&self.superblock.to_bytes())?;

        Ok(self)
    }

    fn index_db(&self) -> crate::Result<(), Error> {

    }
}