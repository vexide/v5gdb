use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectKind {
    Pros,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    kind: ProjectKind,
    root_dir: PathBuf,
}

impl Project {
    pub fn discover(workspace: &Path) -> io::Result<Option<Self>> {
        let pros = workspace.join("project.pros").exists();
        if pros {
            return Ok(Some(Project {
                kind: ProjectKind::Pros,
                root_dir: workspace.into(),
            }));
        }

        if let Some(parent) = workspace.parent() {
            return Self::discover(parent);
        }

        Ok(None)
    }

    pub fn discover_elf_files(&self) -> Vec<PathBuf> {
        let mut candidates = vec![];

        match self.kind {
            ProjectKind::Pros => {
                let candidates_if_hot_cold =
                    ["bin/hot.package.elf", "bin/cold.package.elf"].map(Path::new);
                let candidate_if_monolith = Path::new("bin/monolith.elf");

                let hot_cold = pros::is_hot_cold(&self.root_dir)
                    .unwrap_or_else(|| candidates_if_hot_cold[0].exists());
                if hot_cold {
                    candidates.extend(candidates_if_hot_cold.map(PathBuf::from));
                } else {
                    candidates.push(candidate_if_monolith.into())
                }
            }
        }

        candidates
            .into_iter()
            .map(|cnd| self.root_dir.join(cnd))
            .filter(|cnd| cnd.exists())
            .collect()
    }
}

mod pros {
    use std::path::Path;

    use regex::regex;

    pub fn is_hot_cold(root_dir: &Path) -> Option<bool> {
        let pat = regex!(r"USE_PACKAGE\s*:=\s*(\d)");

        let makefile = root_dir.join("Makefile");
        let contents = std::fs::read_to_string(makefile).ok()?;
        let capture = pat.captures(&contents)?.get(1)?.as_str();

        if capture == "1" {
            Some(true)
        } else if capture == "0" {
            Some(false)
        } else {
            None
        }
    }
}
