use db::BlockDriver;

type Result<T, Error = db::Error> = core::result::Result<T, Error>;

pub fn main() -> Result<()> {
    let file = BlockDriver::blank("/tmp/fs.db")?;

    dbg!(&file.path());

    Ok(())
}