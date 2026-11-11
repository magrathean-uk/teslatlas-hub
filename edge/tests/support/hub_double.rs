#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use teslatlas_edge::protocol::{HubAckV1, HubBatchV1};
use uuid::Uuid;

#[derive(Debug)]
pub struct HubDouble {
    path: PathBuf,
    state: HubState,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HubState {
    version: u16,
    seen_record_ids: BTreeSet<String>,
    applied_txids: Vec<String>,
}

impl HubDouble {
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let state = if path.exists() {
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap()
        } else {
            HubState {
                version: 1,
                ..HubState::default()
            }
        };
        assert_eq!(state.version, 1);
        Self { path, state }
    }

    pub fn commit_before_ack(&mut self, batch: &HubBatchV1) -> HubAckV1 {
        for record in &batch.records {
            if self
                .state
                .seen_record_ids
                .insert(record.record_id.as_str().to_owned())
            {
                self.state.applied_txids.push(record.envelope.txid.clone());
            }
        }
        self.persist();
        let mut accepted_record_ids = batch
            .records
            .iter()
            .map(|record| record.record_id.clone())
            .collect::<Vec<_>>();
        accepted_record_ids.reverse();
        HubAckV1 {
            version: 1,
            batch_id: batch.batch_id.clone(),
            accepted_record_ids,
        }
    }

    pub fn applied_txids(&self) -> &[String] {
        &self.state.applied_txids
    }

    fn persist(&self) {
        let parent = self.path.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        let temporary = parent.join(format!(".hub-double.{}.tmp", Uuid::new_v4()));
        let bytes = serde_json::to_vec(&self.state).unwrap();
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .unwrap();
        output.write_all(&bytes).unwrap();
        output.sync_all().unwrap();
        drop(output);
        fs::rename(temporary, &self.path).unwrap();
        File::open(parent).unwrap().sync_all().unwrap();
    }
}
