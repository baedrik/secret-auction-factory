use serde::Serialize;

use cosmwasm_std::{
    log, to_binary, Api, Binary, CanonicalAddr, Env, Extern, HandleResponse, HandleResult,
    HumanAddr, InitResponse, InitResult, Querier, QueryResult, ReadonlyStorage, StdError,
    StdResult, Storage, Uint128,
};

use cosmwasm_storage::{PrefixedStorage, ReadonlyPrefixedStorage};

use std::collections::{HashMap, HashSet};

use secret_toolkit::{
    snip20::{send_from_msg, token_info_query},
    storage::{AppendStore, AppendStoreMut},
    utils::{pad_handle_result, pad_query_result, InitCallback},
};

use crate::msg::{
    AuctionContractInfo, AuctionInfo, ClosedAuctionInfo, ContractInfo, FilterTypes, HandleAnswer,
    HandleMsg, InitMsg, MyActiveLists, MyClosedLists, QueryAnswer, QueryMsg, RegisterAuctionInfo,
    ResponseStatus::Success, StoreAuctionInfo, StoreClosedAuctionInfo,
};
use crate::rand::sha_256;
use crate::state::{load, may_load, remove, save, Config, TokenSymDec};
use crate::viewing_key::{ViewingKey, VIEWING_KEY_SIZE};

/// prefix for storage of sellers' closed auctions
pub const PREFIX_SELLERS_CLOSED: &[u8] = b"sellersclosed";
/// prefix for storage of sellers' active auctions
pub const PREFIX_SELLERS_ACTIVE: &[u8] = b"sellersactive";
/// prefix for storage of bidders' active auctions
pub const PREFIX_BIDDERS: &[u8] = b"bidders";
/// prefix for storage of bidders' won auctions
pub const PREFIX_WINNERS: &[u8] = b"winners";
/// prefix for storage of an active auction info
pub const PREFIX_ACTIVE_INFO: &[u8] = b"activeinfo";
/// prefix for storage of a closed auction info
pub const PREFIX_CLOSED_INFO: &[u8] = b"closedinfo";
/// prefix for viewing keys
pub const PREFIX_VIEW_KEY: &[u8] = b"viewingkey";
/// storage key for prng seed
pub const PRNG_SEED_KEY: &[u8] = b"prngseed";
/// storage key for the factory config
pub const CONFIG_KEY: &[u8] = b"config";
/// storage key for the active auction list
pub const ACTIVE_KEY: &[u8] = b"active";
/// storage key for token symbols and decimals
pub const SYMDEC_KEY: &[u8] = b"symdec";
/// storage key for the label of the auction we just instantiated
pub const PENDING_KEY: &[u8] = b"pending";
/// pad handle responses and log attributes to blocks of 256 bytes to prevent leaking info based on
/// response size
pub const BLOCK_SIZE: usize = 256;

////////////////////////////////////// Init ///////////////////////////////////////
/// Returns InitResult
///
/// Initializes the factory and creates a prng from the entropy String
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `msg` - InitMsg passed in with the instantiation message
pub fn init<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    msg: InitMsg,
) -> InitResult {
    let prng_seed: Vec<u8> = sha_256(base64::encode(msg.entropy).as_bytes()).to_vec();
    let active: HashSet<u32> = HashSet::new();
    let symdec: Vec<TokenSymDec> = Vec::new();

    let config = Config {
        version: msg.auction_contract,
        symdecmap: HashMap::new(),
        index: 0,
        stopped: false,
        admin: deps.api.canonical_address(&env.message.sender)?,
    };

    save(&mut deps.storage, CONFIG_KEY, &config)?;
    save(&mut deps.storage, PRNG_SEED_KEY, &prng_seed)?;
    save(&mut deps.storage, ACTIVE_KEY, &active)?;
    save(&mut deps.storage, SYMDEC_KEY, &symdec)?;

    Ok(InitResponse::default())
}

///////////////////////////////////// Handle //////////////////////////////////////
/// Returns HandleResult
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `msg` - HandleMsg passed in with the execute message
pub fn handle<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    msg: HandleMsg,
) -> HandleResult {
    let response = match msg {
        HandleMsg::CreateAuction {
            label,
            sell_contract,
            bid_contract,
            sell_amount,
            minimum_bid,
            ends_at,
            description,
        } => try_create_auction(
            deps,
            env,
            label,
            sell_contract,
            bid_contract,
            sell_amount,
            minimum_bid,
            ends_at,
            description,
        ),
        HandleMsg::RegisterAuction {
            seller,
            auction,
            sell_contract,
        } => try_register_auction(deps, env, seller, &auction, sell_contract),
        HandleMsg::RegisterBidder { index, bidder } => try_reg_bidder(deps, env, index, bidder),
        HandleMsg::RemoveBidder { index, bidder } => try_remove_bidder(deps, env, index, &bidder),
        HandleMsg::CloseAuction {
            index,
            seller,
            bidder,
            winning_bid,
        } => try_close_auction(deps, env, index, &seller, bidder.as_ref(), winning_bid),
        HandleMsg::CreateViewingKey { entropy } => try_create_key(deps, env, &entropy),
        HandleMsg::SetViewingKey { key, .. } => try_set_key(deps, env, &key),
        HandleMsg::NewAuctionContract { auction_contract } => {
            try_new_contract(deps, env, auction_contract)
        }
        HandleMsg::SetStatus { stop } => try_set_status(deps, env, stop),
        HandleMsg::ChangeAuctionInfo {
            index,
            ends_at,
            minimum_bid,
        } => try_change_auction_info(deps, env, index, ends_at, minimum_bid),
    };
    pad_handle_result(response, BLOCK_SIZE)
}

/// Returns HandleResult
///
/// create a new auction
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `label` - String containing the label to give the auction
/// * `sell_contract` - ContractInfo containing the code hash and address of the sale token
/// * `bid_contract` - ContractInfo containing the code hash and address of the bid token
/// * `sell_amount` - Uint128 amount to sell in smallest denomination
/// * `minimum_bid` - Uint128 minimum bid owner will accept
/// * `ends_at` - time in seconds since epoch 01/01/1970 after which anyone may close the auction
/// * `description` - optional free-form text string owner may have used to describe the auction
#[allow(clippy::too_many_arguments)]
fn try_create_auction<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    label: String,
    sell_contract: ContractInfo,
    bid_contract: ContractInfo,
    sell_amount: Uint128,
    minimum_bid: Uint128,
    ends_at: u64,
    description: Option<String>,
) -> HandleResult {
    /// Instantiation message
    #[derive(Serialize)]
    pub struct AuctionInitMsg {
        /// factory contract code hash and address
        pub factory: ContractInfo,
        /// auction index with the factory
        pub index: u32,
        /// String label for the auction
        pub label: String,
        /// auction seller
        pub seller: HumanAddr,
        /// sell contract code hash and address
        pub sell_contract: ContractInfo,
        /// sell symbol index
        pub sell_symbol: u16,
        /// sell token decimal places
        pub sell_decimals: u8,
        /// bid contract code hash and address
        pub bid_contract: ContractInfo,
        /// bid symbol index
        pub bid_symbol: u16,
        /// bid token decimal places,
        pub bid_decimals: u8,
        /// amount of tokens being sold
        pub sell_amount: Uint128,
        /// minimum bid that will be accepted
        pub minimum_bid: Uint128,
        /// timestamp after which anyone may close the auction.
        /// Timestamp is in seconds since epoch 01/01/1970
        pub ends_at: u64,
        /// Optional free-form description of the auction (best to avoid double quotes). As an example
        /// it could be the date the owner will likely finalize the auction, or a list of other
        /// auctions for the same token, etc...
        #[serde(default)]
        pub description: Option<String>,
    }

    impl InitCallback for AuctionInitMsg {
        const BLOCK_SIZE: usize = BLOCK_SIZE;
    }

    let mut config: Config = load(&deps.storage, CONFIG_KEY)?;
    if config.stopped {
        return Err(StdError::generic_err(
            "The factory has been stopped.  No new auctions can be created",
        ));
    }

    let factory = ContractInfo {
        code_hash: env.contract_code_hash,
        address: env.contract.address,
    };
    // get sell token info
    let sell_token_info = token_info_query(
        &deps.querier,
        BLOCK_SIZE,
        sell_contract.code_hash.clone(),
        sell_contract.address.clone(),
    )?;
    let sell_decimals = sell_token_info.decimals;
    let sell_addr_raw = &deps.api.canonical_address(&sell_contract.address)?;
    let may_sell_index = config
        .symdecmap
        .get(&sell_addr_raw.as_slice().to_vec())
        .copied();
    // get bid token info
    let bid_token_info = token_info_query(
        &deps.querier,
        BLOCK_SIZE,
        bid_contract.code_hash.clone(),
        bid_contract.address.clone(),
    )?;
    let bid_decimals = bid_token_info.decimals;
    let bid_addr_raw = &deps.api.canonical_address(&bid_contract.address)?;
    let may_bid_index = config
        .symdecmap
        .get(&bid_addr_raw.as_slice().to_vec())
        .copied();
    let add_symbol = may_sell_index.is_none() || may_bid_index.is_none();
    let sell_index: u16;
    let bid_index: u16;
    // if there is a new symbol add it to the list and get its index
    if add_symbol {
        let mut symdecs: Vec<TokenSymDec> = load(&deps.storage, SYMDEC_KEY)?;
        match may_sell_index {
            Some(unwrap) => sell_index = unwrap,
            None => {
                let symdec = TokenSymDec {
                    symbol: sell_token_info.symbol,
                    decimals: sell_token_info.decimals,
                };
                sell_index = symdecs.len() as u16;
                config
                    .symdecmap
                    .insert(sell_addr_raw.as_slice().to_vec(), sell_index);
                symdecs.push(symdec)
            }
        }
        match may_bid_index {
            Some(unwrap) => bid_index = unwrap,
            None => {
                let symdec = TokenSymDec {
                    symbol: bid_token_info.symbol,
                    decimals: bid_token_info.decimals,
                };
                bid_index = symdecs.len() as u16;
                config
                    .symdecmap
                    .insert(bid_addr_raw.as_slice().to_vec(), bid_index);
                symdecs.push(symdec)
            }
        }
        save(&mut deps.storage, SYMDEC_KEY, &symdecs)?;
    // not a new symbol so just get its index from the map
    } else {
        sell_index = may_sell_index.unwrap();
        bid_index = may_bid_index.unwrap();
    }

    // save label and only register an auction giving the matching label
    save(&mut deps.storage, PENDING_KEY, &label)?;

    let initmsg = AuctionInitMsg {
        factory,
        index: config.index,
        label: label.clone(),
        seller: env.message.sender,
        sell_contract,
        sell_symbol: sell_index,
        sell_decimals,
        bid_contract,
        bid_symbol: bid_index,
        bid_decimals,
        sell_amount,
        minimum_bid,
        ends_at,
        description,
    };
    // increment the index for the next auction
    config.index += 1;
    save(&mut deps.storage, CONFIG_KEY, &config)?;

    let cosmosmsg = initmsg.to_cosmos_msg(
        label,
        config.version.code_id,
        config.version.code_hash,
        None,
    )?;

    Ok(HandleResponse {
        messages: vec![cosmosmsg],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::Status {
            status: Success,
            message: None,
        })?),
    })
}

/// Returns HandleResult
///
/// Registers the calling auction by saving its info and adding it to the appropriate lists
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `seller` - reference to the address of the auction's seller
/// * `reg_auction` - reference to RegisterAuctionInfo of the auction that is trying to register
fn try_register_auction<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    seller: HumanAddr,
    reg_auction: &RegisterAuctionInfo,
    sell_contract: ContractInfo,
) -> HandleResult {
    // verify this is the auction we are waiting for
    let load_label: Option<String> = may_load(&deps.storage, PENDING_KEY)?;
    let auth_label =
        load_label.ok_or_else(|| StdError::generic_err("Unable to authenticate registration."))?;
    if auth_label != reg_auction.label {
        return Err(StdError::generic_err(
            "Label does not match the auction we are creating",
        ));
    }
    remove(&mut deps.storage, PENDING_KEY);

    // convert register auction info to storage format
    let auction_addr = deps.api.canonical_address(&env.message.sender)?;
    let auction = reg_auction.to_store_auction_info(auction_addr);

    // save the auction info keyed by its index
    let mut info_store = PrefixedStorage::new(PREFIX_ACTIVE_INFO, &mut deps.storage);
    save(&mut info_store, &reg_auction.index.to_le_bytes(), &auction)?;

    // add the auction address to list of active auctions
    let mut active: HashSet<u32> = load(&deps.storage, ACTIVE_KEY)?;
    active.insert(reg_auction.index);
    save(&mut deps.storage, ACTIVE_KEY, &active)?;

    // get list of seller's active auctions
    let seller_raw = &deps.api.canonical_address(&seller)?;
    let mut seller_store = PrefixedStorage::new(PREFIX_SELLERS_ACTIVE, &mut deps.storage);
    let load_auctions: Option<HashSet<u32>> = may_load(&seller_store, seller_raw.as_slice())?;
    let mut my_active = load_auctions.unwrap_or_default();
    // add this auction to seller's list
    my_active.insert(reg_auction.index);
    save(&mut seller_store, seller_raw.as_slice(), &my_active)?;

    Ok(HandleResponse {
        messages: vec![send_from_msg(
            seller,
            env.message.sender.clone(),
            reg_auction.sell_amount,
            None,
            None,
            BLOCK_SIZE,
            sell_contract.code_hash,
            sell_contract.address,
        )?],
        log: vec![log("auction_address", env.message.sender)],
        data: None,
    })
}

/// Returns HandleResult
///
/// closes the calling auction by saving its info and adding/removing it to/from the
/// appropriate lists
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `index` - auction index
/// * `seller` - reference to the address of the auction's seller
/// * `bidder` - reference to the auction's winner if it had one
/// * `winning_bid` - auction's winning bid if it had one
fn try_close_auction<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    index: u32,
    seller: &HumanAddr,
    bidder: Option<&HumanAddr>,
    winning_bid: Option<Uint128>,
) -> HandleResult {
    let auction_addr = &deps.api.canonical_address(&env.message.sender)?;

    // verify auction is in active list of auctions and not a spam attempt
    let (may_active, may_info, may_error) =
        authenticate_auction(&deps.storage, auction_addr, index)?;
    if let Some(error) = may_error {
        return error;
    }
    // delete the active auction info
    let mut info_store = PrefixedStorage::new(PREFIX_ACTIVE_INFO, &mut deps.storage);
    info_store.remove(&index.to_le_bytes());
    // remove the auction from the active list
    let mut active = may_active.unwrap();
    active.remove(&index);
    save(&mut deps.storage, ACTIVE_KEY, &active)?;

    // set the closed auction info
    let timestamp = env.block.time;
    let auction_info = may_info.unwrap();
    let closed_info =
        auction_info.to_store_closed_auction_info(winning_bid.map(|n| n.u128()), timestamp);
    let mut closed_info_store = PrefixedStorage::new(PREFIX_CLOSED_INFO, &mut deps.storage);
    let mut closed_store = AppendStoreMut::attach_or_create(&mut closed_info_store)?;
    let closed_index = closed_store.len();
    closed_store.push(&closed_info)?;

    // remove auction from seller's active list
    let seller_raw = &deps.api.canonical_address(seller)?;
    remove_from_persons_active(&mut deps.storage, PREFIX_SELLERS_ACTIVE, seller_raw, index)?;
    // add to seller's closed list
    add_to_persons_closed(
        &mut deps.storage,
        PREFIX_SELLERS_CLOSED,
        seller_raw,
        closed_index,
    )?;

    // if auction had a winner
    if let Some(winner) = bidder {
        let winner_raw = &deps.api.canonical_address(winner)?;
        // clean up the bidders list of active auctions
        let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
        let (win_active, _) = filter_only_active(&bidder_store, winner_raw, &mut active)?;
        save(&mut bidder_store, winner_raw.as_slice(), &win_active)?;
        // add to winner's closed list
        add_to_persons_closed(&mut deps.storage, PREFIX_WINNERS, winner_raw, closed_index)?;
    }

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: None,
    })
}

/// Returns HandleResult
///
/// changes the closing time and/or minimum bid of an auction
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `index` - auction index
/// * `ends_at` - optional new closing time
/// * `minimum_bid` - optional new minimum bid
fn try_change_auction_info<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    index: u32,
    ends_at: Option<u64>,
    minimum_bid: Option<Uint128>,
) -> HandleResult {
    let auction_addr = &deps.api.canonical_address(&env.message.sender)?;

    // verify auction is in active list of auctions and not a spam attempt
    let (_may_active, may_info, may_error) =
        authenticate_auction(&deps.storage, auction_addr, index)?;
    if let Some(error) = may_error {
        return error;
    }

    let mut auction_info = may_info.unwrap();
    if let Some(min_bid) = minimum_bid {
        auction_info.minimum_bid = min_bid.u128();
    }
    if let Some(ends) = ends_at {
        auction_info.ends_at = ends;
    }
    let mut info_store = PrefixedStorage::new(PREFIX_ACTIVE_INFO, &mut deps.storage);
    save(&mut info_store, &index.to_le_bytes(), &auction_info)?;
    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: None,
    })
}

/// Returns HandleResult
///
/// registers a new bidder of the calling auction
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `index` - auction index
/// * `bidder` - address of the new bidder
fn try_reg_bidder<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    index: u32,
    bidder: HumanAddr,
) -> HandleResult {
    let auction_addr = &deps.api.canonical_address(&env.message.sender)?;

    // verify auction is in active list of auctions and not a spam attempt
    let (may_active, _may_info, may_error) =
        authenticate_auction(&deps.storage, auction_addr, index)?;
    if let Some(error) = may_error {
        return error;
    }

    let mut active = may_active.unwrap();

    // clean up the bidders list of active auctions
    let bidder_raw = &deps.api.canonical_address(&bidder)?;
    let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
    let (mut my_active, _) = filter_only_active(&bidder_store, bidder_raw, &mut active)?;
    // add this auction to the list
    my_active.insert(index);
    save(&mut bidder_store, bidder_raw.as_slice(), &my_active)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: None,
    })
}

/// Returns HandleResult
///
/// removes registration of the retracting bidder of the calling auction
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `index` - auction index
/// * `bidder` - reference to the address of the retracting bidder
fn try_remove_bidder<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    index: u32,
    bidder: &HumanAddr,
) -> HandleResult {
    let auction_addr = &deps.api.canonical_address(&env.message.sender)?;

    // verify auction is in active list of auctions and not a spam attempt
    let (may_active, _may_info, may_error) =
        authenticate_auction(&deps.storage, auction_addr, index)?;
    if let Some(error) = may_error {
        return error;
    }

    let mut active = may_active.unwrap();

    // clean up the bidders list of active auctions
    let bidder_raw = &deps.api.canonical_address(&bidder)?;
    let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
    let (mut my_active, _) = filter_only_active(&bidder_store, bidder_raw, &mut active)?;
    // remove this auction from the list
    my_active.remove(&index);
    save(&mut bidder_store, bidder_raw.as_slice(), &my_active)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: None,
    })
}

/// Returns StdResult<(Option<HashSet<u32>>, Option<StoreAuctionInfo>, Option<HandleResult>)>
///
/// verifies that the auction is in the list of active auctions, and returns the active auction
/// list, the auction information, or a possible error
///
/// # Arguments
///
/// * `storage` - a reference to contract's storage
/// * `auction` - a reference to the auction's address
/// * `index` - index/key of the auction
#[allow(clippy::type_complexity)]
fn authenticate_auction<S: ReadonlyStorage>(
    storage: &S,
    auction: &CanonicalAddr,
    index: u32,
) -> StdResult<(
    Option<HashSet<u32>>,
    Option<StoreAuctionInfo>,
    Option<HandleResult>,
)> {
    let mut error: Option<HandleResult> = None;
    let mut info: Option<StoreAuctionInfo> = None;
    let active: Option<HashSet<u32>> = may_load(storage, ACTIVE_KEY)?;
    if let Some(active_set) = active.as_ref() {
        // get the auction information
        let info_store = ReadonlyPrefixedStorage::new(PREFIX_ACTIVE_INFO, storage);
        info = may_load(&info_store, &index.to_le_bytes())?;
        if let Some(auction_info) = info.as_ref() {
            if auction_info.address != *auction || !active_set.contains(&index) {
                error = Some(Ok(HandleResponse {
                    messages: vec![],
                    log: vec![log(
                        "Unauthorized",
                        "You are not an active auction this factory created",
                    )],
                    data: None,
                }));
            }
        } else {
            error = Some(Ok(HandleResponse {
                messages: vec![],
                log: vec![
                    log(
                        "Error",
                        "Unable to register action with the factory contract",
                    ),
                    log("Reason", "Missing auction information"),
                ],
                data: None,
            }));
        }
    // if you can't load the active auction list, it is an error but still let auction process
    } else {
        error = Some(Ok(HandleResponse {
            messages: vec![],
            log: vec![
                log(
                    "Error",
                    "Unable to register action with the factory contract",
                ),
                log("Reason", "Missing active auction list"),
            ],
            data: None,
        }));
    }
    Ok((active, info, error))
}

/// Returns HandleResult
///
/// allows admin to add a new auction version to the list of compatible auctions
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `auction_contract` - AuctionContractInfo of the new auction version
fn try_new_contract<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    auction_contract: AuctionContractInfo,
) -> HandleResult {
    // only allow admin to do this
    let mut config: Config = load(&deps.storage, CONFIG_KEY)?;
    let sender = deps.api.canonical_address(&env.message.sender)?;
    if config.admin != sender {
        return Err(StdError::generic_err(
            "This is an admin command. Admin commands can only be run from admin address",
        ));
    }
    config.version = auction_contract;
    save(&mut deps.storage, CONFIG_KEY, &config)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::Status {
            status: Success,
            message: None,
        })?),
    })
}

/// Returns HandleResult
///
/// allows admin to change the factory status to (dis)allow the creation of new auctions
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `stop` - true if the factory should disallow auction creation
fn try_set_status<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    stop: bool,
) -> HandleResult {
    // only allow admin to do this
    let mut config: Config = load(&deps.storage, CONFIG_KEY)?;
    let sender = deps.api.canonical_address(&env.message.sender)?;
    if config.admin != sender {
        return Err(StdError::generic_err(
            "This is an admin command. Admin commands can only be run from admin address",
        ));
    }
    config.stopped = stop;
    save(&mut deps.storage, CONFIG_KEY, &config)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::Status {
            status: Success,
            message: None,
        })?),
    })
}

/// Returns HandleResult
///
/// create a viewing key and set it with any active auctions the sender is the bidder
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `entropy` - string slice to be used as an entropy source for randomization
fn try_create_key<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    entropy: &str,
) -> HandleResult {
    // create and store the key
    let prng_seed: Vec<u8> = load(&deps.storage, PRNG_SEED_KEY)?;
    let key = ViewingKey::new(&env, &prng_seed, entropy.as_ref());
    let message_sender = &deps.api.canonical_address(&env.message.sender)?;
    let mut key_store = PrefixedStorage::new(PREFIX_VIEW_KEY, &mut deps.storage);
    save(&mut key_store, message_sender.as_slice(), &key.to_hashed())?;

    // clean up the bidder's list of active auctions
    let load_active: Option<HashSet<u32>> = may_load(&deps.storage, ACTIVE_KEY)?;
    if let Some(mut active) = load_active {
        let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
        let (my_active, update) = filter_only_active(&bidder_store, message_sender, &mut active)?;
        // if list was updated, save it
        if update {
            save(&mut bidder_store, message_sender.as_slice(), &my_active)?;
        }
    }

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::ViewingKey {
            key: format!("{}", key),
        })?),
    })
}

/// Returns HandleResult
///
/// sets the viewing key and set it with any active auctions the sender is the bidder
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `key` - string slice to be used as the viewing key
fn try_set_key<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    key: &str,
) -> HandleResult {
    // store the viewing key
    let vk = ViewingKey(key.to_string());
    let message_sender = &deps.api.canonical_address(&env.message.sender)?;
    let mut key_store = PrefixedStorage::new(PREFIX_VIEW_KEY, &mut deps.storage);
    save(&mut key_store, message_sender.as_slice(), &vk.to_hashed())?;

    // clean up the bidder's list of active auctions
    let load_active: Option<HashSet<u32>> = may_load(&deps.storage, ACTIVE_KEY)?;
    if let Some(mut active) = load_active {
        let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
        let (my_active, update) = filter_only_active(&bidder_store, message_sender, &mut active)?;
        // if list was updated, save it
        if update {
            save(&mut bidder_store, message_sender.as_slice(), &my_active)?;
        }
    }

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::ViewingKey {
            key: key.to_string(),
        })?),
    })
}

/// Returns StdResult<()>
///
/// add an auction to a seller's or bidder's list of closed auctions
///
/// # Arguments
///
/// * `storage` - mutable reference to contract's storage
/// * `prefix` - prefix to storage of either seller's or bidder's closed auction lists
/// * `person` - a reference to the canonical address of the person the list belongs to
/// * `index` - index of the auction to add
fn add_to_persons_closed<S: Storage>(
    storage: &mut S,
    prefix: &[u8],
    person: &CanonicalAddr,
    index: u32,
) -> StdResult<()> {
    let mut store = PrefixedStorage::new(prefix, storage);
    let load_closed: Option<Vec<u32>> = may_load(&store, person.as_slice())?;
    let mut closed = load_closed.unwrap_or_default();
    closed.push(index);
    save(&mut store, person.as_slice(), &closed)?;
    Ok(())
}

/// Returns StdResult<()>
///
/// remove an auction from a seller's or bidder's list of active auctions
///
/// # Arguments
///
/// * `storage` - mutable reference to contract's storage
/// * `prefix` - prefix to storage of either seller's or bidder's active auction lists
/// * `person` - a reference to the canonical address of the person the list belongs to
/// * `index` - index of the auction to remove
fn remove_from_persons_active<S: Storage>(
    storage: &mut S,
    prefix: &[u8],
    person: &CanonicalAddr,
    index: u32,
) -> StdResult<()> {
    let mut store = PrefixedStorage::new(prefix, storage);
    let load_active: Option<HashSet<u32>> = may_load(&store, person.as_slice())?;
    if let Some(mut active) = load_active {
        active.remove(&index);
        save(&mut store, person.as_slice(), &active)?;
    }
    Ok(())
}

/// Returns StdResult<(HashSet<u32>, bool)> which is the address' updated active list
/// and a bool that is true if the list has been changed from what was in storage
///
/// remove any closed auctions from the list
///
/// # Arguments
///
/// * `storage` - a reference to bidder's active list storage subspace
/// * `address` - a reference to the canonical address of the person the list belongs to
/// * `active` - a mutable reference to the HashSet list of active auctions
fn filter_only_active<S: ReadonlyStorage>(
    storage: &S,
    address: &CanonicalAddr,
    active: &mut HashSet<u32>,
) -> StdResult<(HashSet<u32>, bool)> {
    // get person's current list
    let load_auctions: Option<HashSet<u32>> = may_load(storage, address.as_slice())?;

    // if there are active auctions in the list
    if let Some(my_auctions) = load_auctions {
        let start_len = my_auctions.len();
        // only keep the intersection of the person's list and the active auctions list
        let my_active: HashSet<u32> = my_auctions.iter().filter_map(|v| active.take(v)).collect();
        let updated = start_len != my_active.len();
        return Ok((my_active, updated));
        // if not just return an empty list
    }
    Ok((HashSet::new(), false))
}

/////////////////////////////////////// Query /////////////////////////////////////
/// Returns QueryResult
///
/// # Arguments
///
/// * `deps` - reference to Extern containing all the contract's external dependencies
/// * `msg` - QueryMsg passed in with the query call
pub fn query<S: Storage, A: Api, Q: Querier>(deps: &Extern<S, A, Q>, msg: QueryMsg) -> QueryResult {
    let response = match msg {
        QueryMsg::ListMyAuctions {
            address,
            viewing_key,
            filter,
        } => try_list_my(deps, &address, viewing_key, filter),
        QueryMsg::ListActiveAuctions {} => try_list_active(deps),
        QueryMsg::ListClosedAuctions { before, page_size } => {
            try_list_closed(deps, before, page_size)
        }
        QueryMsg::IsKeyValid {
            address,
            viewing_key,
        } => try_validate_key(deps, &address, viewing_key),
    };
    pad_query_result(response, BLOCK_SIZE)
}

/// Returns QueryResult indicating whether the address/key pair is valid
///
/// # Arguments
///
/// * `deps` - reference to Extern containing all the contract's external dependencies
/// * `address` - a reference to the address whose key should be validated
/// * `viewing_key` - String key used for authentication
fn try_validate_key<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    address: &HumanAddr,
    viewing_key: String,
) -> QueryResult {
    let addr_raw = &deps.api.canonical_address(address)?;
    to_binary(&QueryAnswer::IsKeyValid {
        is_valid: is_key_valid(&deps.storage, addr_raw, viewing_key)?,
    })
}

/// Returns QueryResult listing the active auctions
///
/// # Arguments
///
/// * `deps` - reference to Extern containing all the contract's external dependencies
fn try_list_active<S: Storage, A: Api, Q: Querier>(deps: &Extern<S, A, Q>) -> QueryResult {
    to_binary(&QueryAnswer::ListActiveAuctions {
        active: display_active_list(&deps.api, &deps.storage, None, ACTIVE_KEY)?,
    })
}

/// Returns StdResult<bool> result of validating an address' viewing key
///
/// # Arguments
///
/// * `storage` - a reference to the contract's storage
/// * `address` - a reference to the address whose key should be validated
/// * `viewing_key` - String key used for authentication
fn is_key_valid<S: ReadonlyStorage>(
    storage: &S,
    address: &CanonicalAddr,
    viewing_key: String,
) -> StdResult<bool> {
    // load the address' key
    let read_key = ReadonlyPrefixedStorage::new(PREFIX_VIEW_KEY, storage);
    let load_key: Option<[u8; VIEWING_KEY_SIZE]> = may_load(&read_key, address.as_slice())?;
    let input_key = ViewingKey(viewing_key);
    // if a key was set
    if let Some(expected_key) = load_key {
        // and it matches
        if input_key.check_viewing_key(&expected_key) {
            return Ok(true);
        }
    } else {
        // Checking the key will take significant time. We don't want to exit immediately if it isn't set
        // in a way which will allow to time the command and determine if a viewing key doesn't exist
        input_key.check_viewing_key(&[0u8; VIEWING_KEY_SIZE]);
    }
    Ok(false)
}

/// Returns QueryResult listing the auctions the address interacted with
///
/// # Arguments
///
/// * `deps` - reference to Extern containing all the contract's external dependencies
/// * `address` - a reference to the address whose auctions should be listed
/// * `viewing_key` - String key used to authenticate the query
/// * `filter` - optional choice of display filters
fn try_list_my<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    address: &HumanAddr,
    viewing_key: String,
    filter: Option<FilterTypes>,
) -> QueryResult {
    let addr_raw = &deps.api.canonical_address(address)?;
    // if key matches
    if is_key_valid(&deps.storage, addr_raw, viewing_key)? {
        let mut active_lists: Option<MyActiveLists> = None;
        let mut closed_lists: Option<MyClosedLists> = None;
        // if no filter default to ALL
        let types = filter.unwrap_or(FilterTypes::All);

        // list the active auctions
        if types == FilterTypes::Active || types == FilterTypes::All {
            let seller_active = display_active_list(
                &deps.api,
                &deps.storage,
                Some(PREFIX_SELLERS_ACTIVE),
                addr_raw.as_slice(),
            )?;
            let bidder_active = display_active_list(
                &deps.api,
                &deps.storage,
                Some(PREFIX_BIDDERS),
                addr_raw.as_slice(),
            )?;
            if seller_active.is_some() || bidder_active.is_some() {
                active_lists = Some(MyActiveLists {
                    as_seller: seller_active,
                    as_bidder: bidder_active,
                });
            }
        }
        // list the closed auctions
        if types == FilterTypes::Closed || types == FilterTypes::All {
            let seller_closed = display_addr_closed(
                &deps.api,
                &deps.storage,
                PREFIX_SELLERS_CLOSED,
                addr_raw.as_slice(),
            )?;
            let won = display_addr_closed(
                &deps.api,
                &deps.storage,
                PREFIX_WINNERS,
                addr_raw.as_slice(),
            )?;
            if seller_closed.is_some() || won.is_some() {
                closed_lists = Some(MyClosedLists {
                    as_seller: seller_closed,
                    won,
                });
            }
        }

        return to_binary(&QueryAnswer::ListMyAuctions {
            active: active_lists,
            closed: closed_lists,
        });
    }
    to_binary(&QueryAnswer::ViewingKeyError {
        error: "Wrong viewing key for this address or viewing key not set".to_string(),
    })
}

/// Returns StdResult<Option<Vec<AuctionInfo>>>
///
/// provide the appropriate list of active auctions
///
/// # Arguments
///
/// * `api` - reference to the Api used to convert canonical and human addresses
/// * `storage` - a reference to the contract's storage
/// * `prefix` - optional storage prefix to load from
/// * `key` - storage key to read
fn display_active_list<S: ReadonlyStorage, A: Api>(
    api: &A,
    storage: &S,
    prefix: Option<&[u8]>,
    key: &[u8],
) -> StdResult<Option<Vec<AuctionInfo>>> {
    let load_list: Option<HashSet<u32>> = if let Some(pref) = prefix {
        // reading a person's list
        let read = &ReadonlyPrefixedStorage::new(pref, storage);
        // if reading a bidder's list
        if pref == PREFIX_BIDDERS {
            // read the factory's active list
            let load_active: Option<HashSet<u32>> = may_load(storage, ACTIVE_KEY)?;
            if let Some(mut active) = load_active {
                let canonical = CanonicalAddr(Binary(key.to_vec()));
                // remove any auctions that closed from the list
                let (my_active, _) = filter_only_active(read, &canonical, &mut active)?;
                Some(my_active)
            } else {
                None
            }
        // read a seller's list
        } else {
            may_load(read, key)?
        }
    // read the factory's active list
    } else {
        may_load(storage, key)?
    };
    // turn list of active auctions to a vec of displayable auction infos
    let mut actives = match load_list {
        Some(list) => {
            let mut display_list = Vec::new();
            let read_info = &ReadonlyPrefixedStorage::new(PREFIX_ACTIVE_INFO, storage);
            // get the token symbol strings
            let symdecs: Vec<TokenSymDec> = load(storage, SYMDEC_KEY)?;
            for index in list.iter() {
                // get this auction's info
                let load_info: Option<StoreAuctionInfo> =
                    may_load(read_info, &index.to_le_bytes())?;
                if let Some(info) = load_info {
                    let may_sell_symdec = symdecs.get(info.sell_symbol as usize);
                    if let Some(sell_symdec) = may_sell_symdec {
                        let may_bid_symdec = symdecs.get(info.bid_symbol as usize);
                        if let Some(bid_symdec) = may_bid_symdec {
                            let pair = format!("{}-{}", sell_symdec.symbol, bid_symdec.symbol);
                            display_list.push(AuctionInfo {
                                address: api.human_address(&info.address)?,
                                label: info.label,
                                pair,
                                sell_amount: Uint128(info.sell_amount),
                                sell_decimals: sell_symdec.decimals,
                                minimum_bid: Uint128(info.minimum_bid),
                                bid_decimals: bid_symdec.decimals,
                                ends_at: info.ends_at,
                            });
                        }
                    }
                }
            }
            display_list
        }
        None => Vec::new(),
    };
    if actives.is_empty() {
        return Ok(None);
    }
    // sort it by pair
    actives.sort_by(|a, b| a.pair.cmp(&b.pair));
    Ok(Some(actives))
}

/// Returns StdResult<Option<Vec<ClosedAuctionInfo>>>
///
/// provide the appropriate list of closed auctions
///
/// # Arguments
///
/// * `api` - reference to the Api used to convert canonical and human addresses
/// * `storage` - a reference to the contract's storage
/// * `prefix` - storage prefix to load from
/// * `key` - storage key to read
fn display_addr_closed<S: ReadonlyStorage, A: Api>(
    api: &A,
    storage: &S,
    prefix: &[u8],
    key: &[u8],
) -> StdResult<Option<Vec<ClosedAuctionInfo>>> {
    let read_closed = &ReadonlyPrefixedStorage::new(prefix, storage);
    let load_list: Option<Vec<u32>> = may_load(read_closed, key)?;
    let closed = match load_list {
        Some(list) => {
            let mut display_list = Vec::new();
            let read_store = &ReadonlyPrefixedStorage::new(PREFIX_CLOSED_INFO, storage);
            let may_read_store = AppendStore::<StoreClosedAuctionInfo, _>::attach(read_store);
            if let Some(closed_store) = may_read_store.and_then(|r| r.ok()) {
                // get token symbol strings
                let symdecs: Vec<TokenSymDec> = load(storage, SYMDEC_KEY)?;
                // in reverse chronological order
                for &index in list.iter().rev() {
                    // get this auction's info
                    let load_info = closed_store.get_at(index).ok();
                    if let Some(info) = load_info {
                        let may_sell_symdec = symdecs.get(info.sell_symbol as usize);
                        if let Some(sell_symdec) = may_sell_symdec {
                            let may_bid_symdec = symdecs.get(info.bid_symbol as usize);
                            if let Some(bid_symdec) = may_bid_symdec {
                                let pair = format!("{}-{}", sell_symdec.symbol, bid_symdec.symbol);
                                display_list.push(ClosedAuctionInfo {
                                    index: None,
                                    address: api.human_address(&info.address)?,
                                    label: info.label,
                                    pair,
                                    sell_amount: Uint128(info.sell_amount),
                                    sell_decimals: sell_symdec.decimals,
                                    winning_bid: info.winning_bid.map(Uint128),
                                    bid_decimals: info.winning_bid.map(|_a| bid_symdec.decimals),
                                    timestamp: info.timestamp,
                                });
                            }
                        }
                    }
                }
            }
            display_list
        }
        None => Vec::new(),
    };
    if closed.is_empty() {
        return Ok(None);
    }
    Ok(Some(closed))
}

/// Returns QueryResult listing the closed auctions
///
/// # Arguments
///
/// * `deps` - reference to Extern containing all the contract's external dependencies
/// * `before` - optional u32 index of the earliest auction you do not want to display
/// * `page_size` - optional number of auctions to display
fn try_list_closed<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    before: Option<u32>,
    page_size: Option<u32>,
) -> QueryResult {
    let read_store = &ReadonlyPrefixedStorage::new(PREFIX_CLOSED_INFO, &deps.storage);
    let may_read_store = AppendStore::<StoreClosedAuctionInfo, _>::attach(read_store);
    let mut closed_vec = Vec::new();
    if let Some(closed_store) = may_read_store.and_then(|r| r.ok()) {
        // get the token symbol strings
        let symdecs: Vec<TokenSymDec> = load(&deps.storage, SYMDEC_KEY)?;
        // start iterating from the last close or before given index
        let len = closed_store.len();
        let mut pos = before.unwrap_or(len);
        if pos > len {
            pos = len;
        }
        let skip = (len - pos) as usize;
        let quant = page_size.unwrap_or(200) as usize;
        // grab backwards from the starting point
        for (i, res) in closed_store.iter().enumerate().rev().skip(skip).take(quant) {
            let info = res?;
            let may_sell_symdec = symdecs.get(info.sell_symbol as usize);
            if let Some(sell_symdec) = may_sell_symdec {
                let may_bid_symdec = symdecs.get(info.bid_symbol as usize);
                if let Some(bid_symdec) = may_bid_symdec {
                    let pair = format!("{}-{}", sell_symdec.symbol, bid_symdec.symbol);
                    closed_vec.push(ClosedAuctionInfo {
                        index: Some(i as u32),
                        address: deps.api.human_address(&info.address)?,
                        label: info.label,
                        pair,
                        sell_amount: Uint128(info.sell_amount),
                        sell_decimals: sell_symdec.decimals,
                        winning_bid: info.winning_bid.map(Uint128),
                        bid_decimals: info.winning_bid.map(|_a| bid_symdec.decimals),
                        timestamp: info.timestamp,
                    });
                }
            }
        }
    }
    let closed = if closed_vec.is_empty() {
        None
    } else {
        Some(closed_vec)
    };
    to_binary(&QueryAnswer::ListClosedAuctions { closed })
}
