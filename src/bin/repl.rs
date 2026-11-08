use serde::Serialize;
use serde::Deserialize;
use uuid::Uuid;
use db::TypedDatabase;
use db::items;
use db::keys;
use util::binary::Timestamp;

items! {
    "msg" => struct Message {
        created: Timestamp,
        contents: String,
        from: Uuid,
        to: Uuid,
    }

    "user" => struct User {
        name: String,
        email: String,
        registered: Timestamp,
        oidc_uuid: Uuid,
    }
}

type Result<T, Error = db::Error> = core::result::Result<T, Error>;

pub fn main() -> Result<()> {
    let db = TypedDatabase::new("./fs.db")?
        .with_mapper::<Item>(|obj| match obj {
            Item::Message(msg) => keys![msg.created, msg.from, msg.to],
            Item::User(user) => keys![user.name, user.email, user.oidc_uuid]
        });

    // let obj = db.insert(serde_json::json! {{
    //     "success": true,
    //     "user": {
    //         "name": "JCake",
    //         "email": "jakieschneider13@gmail.com"
    //     }
    // }})?;
    // dbg!(db.search::<serde_json::Value>(obj)?);

    let res = db.query::<Item>("")?;

    dbg!(res);

    Ok(())
}