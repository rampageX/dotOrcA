//! One structured settings file beside the EXE, including language.
//! Existing callback contracts remain unchanged; legacy text is translated
//! at this boundary only, not written to individual files.
use serde_json::{json, Map, Value};
use std::{fs, io, path::Path};

const DOCUMENT: &str = "settings.json";
const LANGUAGE: &str = "ui-language.txt";
const LEGACY: [(&str, &str); 5] = [
    (LANGUAGE, "language"),
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
    if name == LANGUAGE {
        return match raw.trim() {
            "en" => Ok(json!("en")),
            "zh-CN" => Ok(json!("zh-CN")),
            _ => Err(invalid(name)),
        };
    }
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
    if name == LANGUAGE {
        let raw=format!("{}\n",v.as_str().ok_or_else(||invalid(name))?);
        from_legacy(name,&raw)?;
        return Ok(raw);
    }
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
fn parse_document(raw: &str) -> io::Result<Map<String,Value>> {
    serde_json::from_str(raw).map_err(io::Error::other)
}
fn legacy_directory(root: &Path) -> io::Result<Option<std::path::PathBuf>> {
    let dir=root.join("config");
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink()=>Ok(Some(dir)),
        Ok(_)=>Ok(None),
        Err(e) if e.kind()==io::ErrorKind::NotFound=>Ok(None),
        Err(e)=>Err(e),
    }
}
fn remove_migrated(dir:&Path,doc:&Map<String,Value>) -> io::Result<()> {
    // Only delete records whose values are represented in the published file.
    // Keep unrelated files, conflicting records and unreadable data intact.
    for (file,key) in LEGACY {
        if let (Some(value),Some(raw))=(doc.get(key),read_file(dir,file)?) {
            if from_legacy(file,&raw).is_ok_and(|old| &old==value) {
                fs::remove_file(dir.join(file))?;
            }
        }
    }
    if let Some(raw)=read_file(dir,DOCUMENT)? {
        if parse_document(&raw).is_ok_and(|old|old.iter().all(|(k,v)|doc.get(k)==Some(v))) {
            fs::remove_file(dir.join(DOCUMENT))?;
        }
    }
    if fs::read_dir(dir)?.next().is_none() {
        // Non-recursive: never delete unrelated content, including racing writes.
        fs::remove_dir(dir)?;
    }
    Ok(())
}
fn document(root: &Path) -> io::Result<Map<String,Value>> {
    let legacy=legacy_directory(root)?;
    if let Some(raw)=read_file(root,DOCUMENT)? {
        let doc=parse_document(&raw)?;
        if let Some(dir)=legacy.as_deref() { remove_migrated(dir,&doc)?; }
        return Ok(doc);
    }
    let Some(dir)=legacy.as_deref() else { return Ok(Map::new()); };
    // Accept both v1.1.0's five text files and the intermediate candidate
    // JSON + language file. The intermediate JSON takes precedence.
    let old_json=read_file(dir,DOCUMENT)?;
    let mut doc=match old_json.as_deref() {
        Some(raw)=>parse_document(raw)?,
        None=>Map::new(),
    };
    for (file,key) in LEGACY {
        if !doc.contains_key(key) {
            if let Some(raw)=read_file(dir,file)? {
                doc.insert(key.into(),from_legacy(file,&raw)?);
            }
        }
        if let Some(value)=doc.get(key) { to_legacy(file,value)?; }
    }
    if !doc.is_empty() || old_json.is_some() {
        write_document(root,&doc)?;
        remove_migrated(dir,&doc)?;
    } else if fs::read_dir(dir)?.next().is_none() {
        fs::remove_dir(dir)?;
    }
    Ok(doc)
}
pub(super) fn read(root: &Path, name: &str) -> io::Result<Option<String>> {
    document(root)?.get(section(name)?).map(|v|to_legacy(name,v)).transpose()
}
pub(super) fn save(root: &Path, name: &str, value: &str) -> io::Result<()> {
    let mut doc=document(root)?;
    doc.insert(section(name)?.into(),from_legacy(name,value)?);
    write_document(root,&doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_and_independent_updates_preserve_fields() {
        let dir=std::env::temp_dir().join(format!("dotorca-config-{}",std::process::id()));
        let _=fs::remove_dir_all(&dir);
        let legacy=dir.join("config");
        fs::create_dir_all(&legacy).unwrap();
        let output=dir.join("小K & (黑洞) ^ % ! 🎬");
        let dirs=format!("{}\n\n",output.display());
        let old=[
            ("settings.txt","100 68 1 1 3\n"),
            ("directories.txt",dirs.as_str()),
            ("main-placement.txt","-200 80 970 980 0 1\n"),
            ("details-placement.txt","100 200 740 830 0 1\n"),
        ];
        for (name,value) in old {fs::write(legacy.join(name),value).unwrap();}
        fs::write(legacy.join(LANGUAGE),"zh-CN\n").unwrap();
        for (name,value) in old {assert_eq!(read(&dir,name).unwrap().as_deref(),Some(value));}
        assert_eq!(fs::read_dir(&dir).unwrap().count(),1);
        assert!(!legacy.exists());
        let doc:Value=serde_json::from_str(&fs::read_to_string(dir.join(DOCUMENT)).unwrap()).unwrap();
        assert_eq!(doc["language"],"zh-CN");
        assert_eq!(doc["encoding"]["percent"],"68");
        assert_eq!(doc["main_window"]["x"],-200);
        assert_eq!(doc["directories"]["output"].as_str(),output.to_str());
        save(&dir,"main-placement.txt","0 0 880 640 1 1\n").unwrap();
        for (name,value) in old {if name!="main-placement.txt" {assert_eq!(read(&dir,name).unwrap().as_deref(),Some(value));}}
        assert_eq!(read(&dir,LANGUAGE).unwrap().as_deref(),Some("zh-CN\n"));
        assert_eq!(read(&dir,"main-placement.txt").unwrap().as_deref(),Some("0 0 880 640 1 1\n"));
        // Interrupted migration cleanup: only remove identical old records.
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("settings.txt"),old[0].1).unwrap();
        fs::write(legacy.join("unrelated.txt"),"keep").unwrap();
        assert!(read(&dir,"settings.txt").unwrap().is_some());
        assert!(!legacy.join("settings.txt").exists());
        assert!(legacy.join("unrelated.txt").exists());
        save(&dir,LANGUAGE,"en\n").unwrap();
        assert_eq!(read(&dir,LANGUAGE).unwrap().as_deref(),Some("en\n"));
        // A corrupt new file is not overwritten by a later save.
        fs::write(dir.join(DOCUMENT),"{bad").unwrap();
        assert!(save(&dir,"settings.txt","100 50 0 1 0\n").is_err());
        assert_eq!(fs::read_to_string(dir.join(DOCUMENT)).unwrap(),"{bad");
        fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn migrate_intermediate_json_and_language() {
        let root=std::env::temp_dir().join(format!("dotorca-config-intermediate-{}",std::process::id()));
        let _=fs::remove_dir_all(&root);
        let legacy=root.join("config");
        fs::create_dir_all(&legacy).unwrap();
        let old=json!({"encoding":{"mib":"100","percent":"50","mode":1,"codec":1,"resolution":3},"extra":123});
        fs::write(legacy.join(DOCUMENT),old.to_string()).unwrap();
        fs::write(legacy.join(LANGUAGE),"en\n").unwrap();
        assert_eq!(read(&root,LANGUAGE).unwrap().as_deref(),Some("en\n"));
        assert!(!legacy.exists());
        let doc:Value=serde_json::from_str(&fs::read_to_string(root.join(DOCUMENT)).unwrap()).unwrap();
        assert_eq!(doc["extra"],123);
        assert_eq!(doc["encoding"],old["encoding"]);
        assert_eq!(doc["language"],"en");
        fs::remove_dir_all(&root).unwrap();
    }

}
