//! natives 解压。
//!
//! 把版本允许的本地库 jar 解压到 `natives/<version>/<os>`。
//! 解压前使用 `enclosed_name` 防止路径穿越，并应用库的 `extract.exclude`。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use arkmcl_core::meta::{Context, Library, Version, allowed, maven_path};
use zip::ZipArchive;

/// 解压结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 处理的 jar 数量。
    pub jars: usize,
    /// 写出的文件数量。
    pub files: usize,
}

/// 解压错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
    /// ZIP 解析错误。
    Zip {
        /// 出错的 jar。
        path: PathBuf,
        /// 底层错误。
        source: zip::result::ZipError,
    },
    /// 本地库 jar 不存在。
    Missing(PathBuf),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Zip { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Missing(path) => write!(formatter, "missing natives jar: {}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Zip { source, .. } => Some(source),
            Self::Missing(_) => None,
        }
    }
}

/// natives 解压器。
pub struct Extractor {
    minecraft: PathBuf,
}

impl Extractor {
    /// 创建解压器。
    #[must_use]
    pub fn new(minecraft: PathBuf) -> Self {
        Self { minecraft }
    }

    /// 解压版本对应的 natives。
    ///
    /// # Errors
    ///
    /// jar 缺失、ZIP 损坏或写入失败时返回错误。
    pub fn extract(&self, version: &Version, context: &Context) -> Result<Report, Error> {
        let directory = self
            .minecraft
            .join("natives")
            .join(&version.id)
            .join(&context.os);
        let mut report = Report::default();
        for library in &version.libraries {
            if !allowed(&library.rules, context) {
                continue;
            }
            let Some(classifier) = library
                .natives
                .as_ref()
                .and_then(|natives| natives.get(&context.os))
            else {
                continue;
            };
            let jar = self.native_path(library, classifier)?;
            if !jar.exists() {
                return Err(Error::Missing(jar));
            }
            let exclude = library
                .extract
                .as_ref()
                .map_or_else(Vec::new, |extract| extract.exclude.clone());
            let files = Self::extract_jar(&jar, &directory, &exclude)?;
            report.jars += 1;
            report.files += files;
        }
        Ok(report)
    }

    fn native_path(&self, library: &Library, classifier: &str) -> Result<PathBuf, Error> {
        let relative = library
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.classifiers.as_ref())
            .and_then(|classifiers| classifiers.get(classifier))
            .and_then(|artifact| artifact.path.clone())
            .or_else(|| maven_path(&library.name, Some(classifier)))
            .ok_or_else(|| Error::Missing(self.minecraft.join("libraries").join(&library.name)))?;
        Ok(self.minecraft.join("libraries").join(relative))
    }

    fn extract_jar(jar: &Path, directory: &Path, exclude: &[String]) -> Result<usize, Error> {
        let file = fs::File::open(jar).map_err(|source| Error::Io {
            path: jar.to_path_buf(),
            source,
        })?;
        let mut archive = ZipArchive::new(file).map_err(|source| Error::Zip {
            path: jar.to_path_buf(),
            source,
        })?;
        fs::create_dir_all(directory).map_err(|source| Error::Io {
            path: directory.to_path_buf(),
            source,
        })?;

        let mut files = 0;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|source| Error::Zip {
                path: jar.to_path_buf(),
                source,
            })?;
            if entry.is_dir() {
                continue;
            }
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            let name_text = name.to_string_lossy().replace('\\', "/");
            if exclude.iter().any(|prefix| name_text.starts_with(prefix)) {
                continue;
            }
            let target = directory.join(&name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|source| Error::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            let mut output = fs::File::create(&target).map_err(|source| Error::Io {
                path: target.clone(),
                source,
            })?;
            io::copy(&mut entry, &mut output).map_err(|source| Error::Io {
                path: target,
                source,
            })?;
            files += 1;
        }
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::Extractor;
    use arkmcl_core::meta::{Context, Version};
    use std::collections::HashMap;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("arkmcl-natives-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn context() -> Context {
        Context {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            features: HashMap::new(),
        }
    }

    #[test]
    fn extracts_with_exclusions() {
        let directory = temp_dir("extract");
        let jar = directory.join("libraries/org/example/native/1.0/native-1.0-natives-linux.jar");
        fs::create_dir_all(jar.parent().expect("parent")).expect("libraries");
        let file = fs::File::create(&jar).expect("jar");
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("META-INF/MANIFEST.MF", options)
            .expect("manifest");
        zip.write_all(b"manifest").expect("write");
        zip.start_file("libexample.so", options).expect("library");
        zip.write_all(b"binary").expect("write");
        zip.finish().expect("finish");

        let version: Version = serde_json::from_str(
            r#"{
              "id": "1.20.4",
              "type": "release",
              "mainClass": "net.minecraft.client.main.Main",
              "libraries": [{
                "name": "org.example:native:1.0",
                "natives": {"linux": "natives-linux"},
                "downloads": {"classifiers": {"natives-linux": {"path": "org/example/native/1.0/native-1.0-natives-linux.jar", "sha1": "", "size": 0, "url": ""}}},
                "extract": {"exclude": ["META-INF/"]}
              }]
            }"#,
        )
        .expect("version");

        let report = Extractor::new(directory.clone())
            .extract(&version, &context())
            .expect("extract");
        assert_eq!(report.jars, 1);
        assert_eq!(report.files, 1);
        assert!(
            directory
                .join("natives/1.20.4/linux/libexample.so")
                .exists()
        );
        assert!(!directory.join("natives/1.20.4/linux/META-INF").exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn reports_missing_jar() {
        let directory = temp_dir("missing");
        let version: Version = serde_json::from_str(
            r#"{
              "id": "1.20.4",
              "type": "release",
              "mainClass": "net.minecraft.client.main.Main",
              "libraries": [{
                "name": "org.example:native:1.0",
                "natives": {"linux": "natives-linux"}
              }]
            }"#,
        )
        .expect("version");
        assert!(
            Extractor::new(directory.clone())
                .extract(&version, &context())
                .is_err()
        );
        let _ = fs::remove_dir_all(&directory);
    }
}
