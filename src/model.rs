use fs::{create_dir_all, hard_link, read_to_string};
use std::collections::HashSet;
use std::error::Error;
use std::fs::read_link;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::{env, fs};

use anyhow::{anyhow, Result};
use atomicwrites::{AllowOverwrite, AtomicFile};
use dirs::home_dir;
use log::{error, info};
use serde::{Deserialize, Serialize};
use serde_json::{from_str, to_string};

use crate::constant::HBX_HOME_ENV;
use crate::model::Meta::{DIRECTORY, FILE, SYMLINK};
use crate::util::md5;
use crate::{constant, util};

#[derive(Debug, Deserialize, Serialize)]
enum Meta {
    FILE(String),
    SYMLINK(PathBuf),
    DIRECTORY(Vec<Node>),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Node {
    name: String,
    meta: Meta,
}

impl PartialEq for Node {
    /// 判断节点是否相同，在linux中可以通过inode判断,此处可以优化
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Node {}

impl Hash for Node {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // 每个元素都有hash方法吗
        self.name.hash(state);
    }
}

impl TryFrom<&Path> for Node {
    type Error = anyhow::Error;

    fn try_from(p: &Path) -> Result<Self, Self::Error> {
        let name = p
            .file_name()
            .ok_or(anyhow!("invalid path"))?
            .to_string_lossy()
            .to_string();

        let meta = if p.is_symlink() {
            SYMLINK(read_link(p)?)
        } else if p.is_dir() {
            DIRECTORY(Vec::new())
        } else {
            FILE(md5(p))
        };

        let n = Self { name, meta };
        Ok(n)
    }
}

impl Node {
    fn sample(s: &str) -> Self {
        Self {
            name: s.to_string(),
            meta: FILE(String::new()),
        }
    }

    fn recursive_link_and_calc(p: &Path, s: &Path) -> Result<Node> {
        let name = p.file_name().unwrap().to_string_lossy().to_string();

        let meta = if p.is_symlink() {
            SYMLINK(p.read_link().unwrap())
        } else if p.is_dir() {
            let mut children = Vec::new();
            for entry in walkdir::WalkDir::new(p)
                .follow_links(false)
                .sort_by_file_name()
                .max_depth(1)
                .into_iter()
                .filter_map(|f| f.ok())
                .filter(|f| f.path() != p)
            {
                let child = Node::recursive_link_and_calc(entry.path(), s)?;
                children.push(child);
            }
            DIRECTORY(children)
        } else {
            let m = util::md5(&p);
            let dst = s.join(Path::new(&m));
            info!("l {:?} -> {:?}", &p, &dst);
            hard_link(&p, &dst)?;
            FILE(m)
        };
        Ok(Node { name, meta })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StoreConfig {
    path: PathBuf,
    data: HashSet<Node>,
}

impl StoreConfig {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            data: HashSet::new(),
        }
    }

    fn default() -> Result<Self> {
        let p = env::var(HBX_HOME_ENV);
        let hbx_home_path: Option<PathBuf> = match p {
            Ok(p) => Some(p.into()),
            Err(_) => home_dir().map(|f| f.join(PathBuf::from(".hbx"))),
        };

        let path = hbx_home_path.unwrap_or(PathBuf::from("~/.hbx"));
        create_dir_all(path.join(constant::STORE_DIRECTORY))?;

        let s = Self {
            path,
            data: HashSet::new(),
        };
        Ok(s)
    }

    fn config_path(&self) -> PathBuf {
        self.path.join(Path::new(constant::CONFIG_NAME))
    }

    fn store_dir(&self) -> PathBuf {
        self.path.join(Path::new(constant::STORE_DIRECTORY))
    }

    /// 加载数据
    fn load(&mut self) -> Result<()> {
        let config_path = self.config_path();
        if config_path.exists() {
            let content = read_to_string(&config_path)?;
            let tmp: HashSet<Node> = from_str(&content)?;
            self.data.extend(tmp);
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let s = to_string(&self.data)?;
        AtomicFile::new(self.config_path(), AllowOverwrite).write(|f| f.write_all(s.as_bytes()))?;
        info!("save path is {}", self.config_path().display());
        Ok(())
    }

    fn add(&mut self, path: &Path) -> Result<()> {
        if path.exists() {
            if !self.data.contains(&path.try_into()?) {
                let node = Node::recursive_link_and_calc(path, &self.store_dir())?;
                self.data.insert(node);
            }
        } else {
            error!("path {:?} not exists, existing", path);
        }
        Ok(())
    }

    fn list(&self) -> Vec<&str> {
        let mut ans = Vec::new();
        for x in &self.data {
            ans.push(x.name.as_str());
        }
        ans
    }

    fn delete(&mut self, name: &str) {
        self.data.remove(&Node {
            name: name.to_owned(),
            meta: FILE("".to_string()),
        });
    }
}

#[cfg(test)]
mod model_test {
    use std::collections::HashSet;
    use std::fs::{create_dir_all, remove_dir_all};
    use std::path::Path;

    use crate::model::StoreConfig;

    #[test]
    fn test_model() -> anyhow::Result<()> {
        let mut config = StoreConfig::default()?;
        config.load()?;
        config.add(Path::new(".idea"))?;
        config.add(Path::new(".idea"))?;
        config.add(Path::new(".idea"))?;
        assert_eq!(1, config.data.len());
        config.save()?;
        // test loading
        config.load()?;
        config.add(Path::new("src"))?;
        assert_eq!(config.data.len(), 2);
        // test delete
        config.delete("src");
        assert_eq!(config.data.len(), 1);
        remove_dir_all(config.path)?;
        Ok(())
    }

    #[test]
    fn delete_all_dirs_test() -> anyhow::Result<()> {
        let config = StoreConfig::default()?;
        remove_dir_all(config.path)?;
        Ok(())
    }

    #[test]
    fn test_hash_set_extend() {
        let ans = [1, 2, 3];
        let mut set = HashSet::new();
        set.extend(ans);
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn create_dirs() -> anyhow::Result<()> {
        let a = Path::new("target/test");
        create_dir_all(a)?;
        create_dir_all(a)?;
        assert!(a.exists());
        remove_dir_all(a)?;
        assert!(!a.exists());
        Ok(())
    }
}