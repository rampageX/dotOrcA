//! One structured settings file, with the language choice kept separate.
//! Existing callback contracts remain unchanged; legacy text is translated
//! at this boundary only, not written to individual files.
use serde_json::{json, Map, Value};
use std::{fs, io, path::Path};

const DOCUMENT: &str = "settings.json";
const LANGUAGE: &str = "ui-language.txt";
const LEGACY: [(&str, &str); 4] = [
    ("settings.txt", "encoding"),
    ("directories.txt", "directories"),
    ("main-placement.txt", "main_window"),
    ("details-placement.txt", "details_window"),
];

fn invalid(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, name.to_owned())
}
fn read_file(dir: &Path, name: &str) -> io::Result<Option<String>> {
    match fs::read_to_string(dir.join(name)) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn write_file(dir: &Path, name: &str, value: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        fs::write(&temporary, value)?;
        fs::rename(&temporary, &path)
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result
}
fn section(name: &str) -> io::Result<&'static str> {
    LEGACY.iter().find(|(file, _)| *file == name).map(|(_, key)| *key)
        .ok_or_else(|| invalid(name))
}
fn number(s: &str, name: &str) -> io::Result<i32> {
    s.parse().map_err(|_| invalid(name))
}
fn from_legacy(name: &str, raw: &str) -> io::Result<Value> {
    if name == "directories.txt" {
        let (output, tools) = raw.split_once('\n').ok_or_else(|| invalid(name))?;
        let output = output.trim_end_matches('\r');
        let tools = tools.trim_end_matches(['\r', '\n']);
        super::parse_paths(output, tools).map_err(|_| invalid(name))?;
        return Ok(json!({"output":output, "ffmpeg":tools}));
    }
    let parts: Vec<_> = raw.split_whitespace().collect();
    if name == "settings.txt" {
        let [mib, percent, mode, codec, resolution] = parts.as_slice() else { return Err(invalid(name)); };
        let (mode, codec, resolution) = (number(mode,name)?,number(codec,name)?,number(resolution,name)?);
        if !super::settings_valid(mib,percent,mode,codec,resolution) { return Err(invalid(name)); }
        return Ok(json!({"mib":mib,"percent":percent,"mode":mode,"codec":codec,"resolution":resolution}));
    }
    let [x,y,width,height,state,has_position] = parts.as_slice() else { return Err(invalid(name)); };
    let p=super::Placement{x:number(x,name)?,y:number(y,name)?,width:number(width,name)?,
        height:number(height,name)?,state:number(state,name)?,has_position:number(has_position,name)?};
    if !p.valid() { return Err(invalid(name)); }
    Ok(json!({"x":p.x,"y":p.y,"width":p.width,"height":p.height,"state":p.state,"has_position":p.has_position}))
}
fn to_legacy(name: &str, v: &Value) -> io::Result<String> {
    let string = |key: &str| v.get(key).and_then(Value::as_str).ok_or_else(|| invalid(name));
    let num = |key: &str| v.get(key).and_then(Value::as_i64).and_then(|n| i32::try_from(n).ok()).ok_or_else(||invalid(name));
    let raw = match name {
        "directories.txt" => format!("{}\n{}\n", string("output")?, string("ffmpeg")?),
        "settings.txt" => format!("{} {} {} {} {}\n",string("mib")?,string("percent")?,num("mode")?,num("codec")?,num("resolution")?),
        _ => format!("{} {} {} {} {} {}\n",num("x")?,num("y")?,num("width")?,num("height")?,num("state")?,num("has_position")?),
    };
    // Reject invalid values rather than silently rewriting a damaged config.
    from_legacy(name,&raw)?;
    Ok(raw)
}
fn write_document(dir: &Path, doc: &Map<String,Value>) -> io::Result<()> {
    let mut data=serde_json::to_string_pretty(doc).map_err(io::Error::other)?;
    data.push('\n');
    write_file(dir,DOCUMENT,&data)
}
fn document(dir: &Path) -> io::Result<Map<String,Value>> {
    if let Some(raw) = read_file(dir,DOCUMENT)? {
        let doc: Map<String,Value> = serde_json::from_str(&raw).map_err(io::Error::other)?;
        // If migration was interrupted after publication, remove only legacy
        // files whose values are already safely represented in the new file.
        for (file,key) in LEGACY {
            if let Some(value)=doc.get(key) {
                if let Some(raw)=read_file(dir,file)? {
                    if from_legacy(file,&raw).is_ok_and(|old|old==*value) {
                        fs::remove_file(dir.join(file))?;
                    }
                }
            }
        }
        return Ok(doc);
    }
    let mut doc=Map::new();
    let mut migrated=Vec::new();
    for (file,key) in LEGACY {
        if let Some(raw)=read_file(dir,file)? {
            doc.insert(key.into(),from_legacy(file,&raw)?);
            migrated.push(file);
        }
    }
    if !migrated.is_empty() {
        // Never remove an old file until every section has been validated and
        // the new document has been atomically published.
        write_document(dir,&doc)?;
        for file in migrated { fs::remove_file(dir.join(file))?; }
    }
    Ok(doc)
}
pub(super) fn read(dir: &Path, name: &str) -> io::Result<Option<String>> {
    if name == LANGUAGE { return read_file(dir,name); }
    document(dir)?.get(section(name)?).map(|v|to_legacy(name,v)).transpose()
}
pub(super) fn save(dir: &Path, name: &str, value: &str) -> io::Result<()> {
    if name == LANGUAGE { return write_file(dir,name,value); }
    let mut doc=document(dir)?;
    doc.insert(section(name)?.into(),from_legacy(name,value)?);
    write_document(dir,&doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_and_independent_updates_preserve_fields() {
        let dir=std::env::temp_dir().join(format!("dotorca-config-{}",std::process::id()));
        let _=fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let output=dir.join("小K & (黑洞) ^ % ! 🎬");
        let dirs=format!("{}\n\n",output.display());
        let old=[
            ("settings.txt","100 68 1 1 3\n"),
            ("directories.txt",dirs.as_str()),
            ("main-placement.txt","-200 80 970 980 0 1\n"),
            ("details-placement.txt","100 200 740 830 0 1\n"),
        ];
        for (name,value) in old {fs::write(dir.join(name),value).unwrap();}
        fs::write(dir.join(LANGUAGE),"zh-CN\n").unwrap();
        for (name,value) in old {assert_eq!(read(&dir,name).unwrap().as_deref(),Some(value));}
        assert_eq!(fs::read_dir(&dir).unwrap().count(),2);
        let doc:Value=serde_json::from_str(&fs::read_to_string(dir.join(DOCUMENT)).unwrap()).unwrap();
        assert_eq!(doc["encoding"]["percent"],"68");
        assert_eq!(doc["main_window"]["x"],-200);
        assert_eq!(doc["directories"]["output"].as_str(),output.to_str());
        save(&dir,"main-placement.txt","0 0 880 640 1 1\n").unwrap();
        for (name,value) in old {if name!="main-placement.txt" {assert_eq!(read(&dir,name).unwrap().as_deref(),Some(value));}}
        assert_eq!(read(&dir,LANGUAGE).unwrap().as_deref(),Some("zh-CN\n"));
        assert_eq!(read(&dir,"main-placement.txt").unwrap().as_deref(),Some("0 0 880 640 1 1\n"));
        // Interrupted migration cleanup: only remove identical old records.
        fs::write(dir.join("settings.txt"),old[0].1).unwrap();
        assert!(read(&dir,"settings.txt").unwrap().is_some());
        assert!(!dir.join("settings.txt").exists());
        // A corrupt new file is not overwritten by a later save.
        fs::write(dir.join(DOCUMENT),"{bad").unwrap();
        assert!(save(&dir,"settings.txt","100 50 0 1 0\n").is_err());
        assert_eq!(fs::read_to_string(dir.join(DOCUMENT)).unwrap(),"{bad");
        fs::remove_dir_all(&dir).unwrap();
    }
}
