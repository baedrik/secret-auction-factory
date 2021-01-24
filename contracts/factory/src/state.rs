use std::any::type_name;
use std::collections::HashMap;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use cosmwasm_std::{CanonicalAddr, ReadonlyStorage, StdError, StdResult, Storage};

use secret_toolkit::serialization::{Bincode2, Serde};

use crate::msg::AuctionContractInfo;

/// symbol and number of decimal places of a token
#[derive(Serialize, Deserialize)]
pub struct TokenSymDec {
    /// token symbol
    pub symbol: String,
    /// number of decimal places for the token
    pub decimals: u8,
}

/// grouping the data primarily used when creating a new auction
#[derive(Serialize, Deserialize)]
pub struct Config {
    /// code hash and address of the auction contract
    pub version: AuctionContractInfo,
    /// map token contract address to symdec list index
    pub symdecmap: HashMap<Vec<u8>, u16>,
    /// unique id to give created auction
    pub index: u32,
    /// factory's create auction status
    pub stopped: bool,
    /// address of the factory admin
    pub admin: CanonicalAddr,
}

/// Returns StdResult<()> resulting from saving an item to storage
///
/// # Arguments
///
/// * `storage` - a mutable reference to the storage this item should go to
/// * `key` - a byte slice representing the key to access the stored item
/// * `value` - a reference to the item to store
pub fn save<T: Serialize, S: Storage>(storage: &mut S, key: &[u8], value: &T) -> StdResult<()> {
    storage.set(key, &Bincode2::serialize(value)?);
    Ok(())
}

/// Removes an item from storage
///
/// # Arguments
///
/// * `storage` - a mutable reference to the storage this item is in
/// * `key` - a byte slice representing the key that accesses the stored item
pub fn remove<S: Storage>(storage: &mut S, key: &[u8]) {
    storage.remove(key);
}

/// Returns StdResult<T> from retrieving the item with the specified key.  Returns a
/// StdError::NotFound if there is no item with that key
///
/// # Arguments
///
/// * `storage` - a reference to the storage this item is in
/// * `key` - a byte slice representing the key that accesses the stored item
pub fn load<T: DeserializeOwned, S: ReadonlyStorage>(storage: &S, key: &[u8]) -> StdResult<T> {
    Bincode2::deserialize(
        &storage
            .get(key)
            .ok_or_else(|| StdError::not_found(type_name::<T>()))?,
    )
}

/// Returns StdResult<Option<T>> from retrieving the item with the specified key.
/// Returns Ok(None) if there is no item with that key
///
/// # Arguments
///
/// * `storage` - a reference to the storage this item is in
/// * `key` - a byte slice representing the key that accesses the stored item
pub fn may_load<T: DeserializeOwned, S: ReadonlyStorage>(
    storage: &S,
    key: &[u8],
) -> StdResult<Option<T>> {
    match storage.get(key) {
        Some(value) => Bincode2::deserialize(&value).map(Some),
        None => Ok(None),
    }
}
