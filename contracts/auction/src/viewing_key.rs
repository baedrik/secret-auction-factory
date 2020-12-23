use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::utils::{create_hashed_password, ct_slice_compare};

pub const VIEWING_KEY_SIZE: usize = 32;

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct ViewingKey(pub String);

impl ViewingKey {
    pub fn check_viewing_key(&self, hashed_pw: &[u8]) -> bool {
        let mine_hashed = create_hashed_password(&self.0);

        ct_slice_compare(&mine_hashed, hashed_pw)
    }

    pub fn to_hashed(&self) -> [u8; VIEWING_KEY_SIZE] {
        create_hashed_password(&self.0)
    }
}

impl fmt::Display for ViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
