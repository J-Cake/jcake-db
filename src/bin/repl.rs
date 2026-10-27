use db::File;

type Result<T, Error = db::Error> = core::result::Result<T, Error>;

pub fn main() -> Result<()> {
    let file = File::initialise("/tmp/fs.db")?;

    dbg!(&file.path());

    Ok(())
}