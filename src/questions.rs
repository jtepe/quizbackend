use crate::messages::{Question, QuestionOption};
use rand::seq::{IndexedRandom, SliceRandom};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct StoredQuestion {
    pub question: Question,
    pub correct_answer_id: String,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    topic: String,
    questions: Vec<RawQuestion>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawQuestion {
    id: String,
    topic: String,
    prompt: String,
    options: Vec<QuestionOption>,
    correct_answer_id: String,
}

pub struct QuestionRegistry {
    pools: HashMap<String, Vec<StoredQuestion>>,
}

impl QuestionRegistry {
    pub fn load_from_dir(dir: &Path) -> Result<Self, String> {
        let mut pools: HashMap<String, Vec<StoredQuestion>> = HashMap::new();
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            let raw: RawFile = serde_json::from_slice(&bytes)
                .map_err(|e| format!("parse {}: {e}", path.display()))?;
            let stored: Vec<StoredQuestion> = raw
                .questions
                .into_iter()
                .map(|q| StoredQuestion {
                    question: Question {
                        id: q.id,
                        topic: q.topic,
                        prompt: q.prompt,
                        options: q.options,
                    },
                    correct_answer_id: q.correct_answer_id,
                })
                .collect();
            pools.insert(raw.topic, stored);
        }
        Ok(Self { pools })
    }

    pub fn pool_size(&self, topic: &str) -> Option<usize> {
        self.pools.get(topic).map(|v| v.len())
    }

    pub fn pick(&self, topic: &str, n: usize) -> Option<Vec<StoredQuestion>> {
        let pool = self.pools.get(topic)?;
        if n == 0 || n > pool.len() {
            return None;
        }
        let mut rng = rand::rng();
        let mut chosen: Vec<StoredQuestion> = pool
            .sample(&mut rng, n)
            .cloned()
            .collect();
        chosen.shuffle(&mut rng);
        Some(chosen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("questions")
    }

    #[test]
    fn loads_known_topics() {
        let reg = QuestionRegistry::load_from_dir(&registry_path()).expect("loads");
        assert!(reg.pool_size("Science").unwrap_or(0) > 0);
        assert!(reg.pool_size("Nerd Stuff").unwrap_or(0) > 0);
        assert!(reg.pool_size("Geography").unwrap_or(0) > 0);
    }

    #[test]
    fn pick_respects_bounds() {
        let reg = QuestionRegistry::load_from_dir(&registry_path()).expect("loads");
        let science_size = reg.pool_size("Science").unwrap();
        assert!(reg.pick("Science", 0).is_none());
        assert!(reg.pick("Science", science_size + 1).is_none());
        assert_eq!(reg.pick("Science", 3).unwrap().len(), 3);
        assert!(reg.pick("Unknown Topic", 1).is_none());
    }
}
