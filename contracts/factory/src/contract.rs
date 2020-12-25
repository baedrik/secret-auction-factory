use serde::Serialize;

use cosmwasm_std::{
    log, to_binary, Api, Binary, CanonicalAddr, Env, Extern, HandleResponse, HandleResult,
    HumanAddr, InitResponse, InitResult, Querier, QueryResult, ReadonlyStorage,
    StdError, StdResult, Storage, Uint128,
};

use cosmwasm_storage::{PrefixedStorage, ReadonlyPrefixedStorage};

use std::collections::{BTreeMap, HashMap, HashSet};

use secret_toolkit::{
    snip20::token_info_query,
    utils::{pad_handle_result, pad_query_result, HandleCallback, InitCallback},
};

use crate::msg::{
    AuctionContractInfo, AuctionInfo, ContractInfo, HandleAnswer, HandleMsg, InitMsg, QueryAnswer,
    QueryMsg, FilterTypes, ClosedAuctionInfo, MyActiveLists, MyClosedLists,
    ResponseStatus::Success,
    StoreAuctionInfo, StoreClosedAuctionInfo,
};
use crate::rand::sha_256;
use crate::state::{load, may_load, remove, save};
use crate::viewing_key::ViewingKey;

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
/// storage key for the factory admin
pub const ADMIN_KEY: &[u8] = b"admin";
/// storage key for the factory status
pub const STATUS_KEY: &[u8] = b"status";
/// storage key for the auction contract info
pub const VERSION_KEY: &[u8] = b"version";
/// storage key for the active auction list
pub const ACTIVE_KEY: &[u8] = b"active";
/// storage key for the closed auction list
pub const CLOSED_KEY: &[u8] = b"closed";
/// storage key for token symbols
pub const SYMBOLS_KEY: &[u8] = b"symbols";
/// storage key for token symbols map
pub const SYMBOLS_MAP_KEY: &[u8] = b"symbolsmap";
/// storage key for the label of the auction we just instantiated
pub const PENDING_KEY: &[u8] = b"pending";
/// pad handle responses and log attributes to blocks of 256 bytes to prevent leaking info based on
/// response size
pub const BLOCK_SIZE: usize = 256;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetKeyMsg {
    /// an auction's SetViewingKey message format
    SetViewingKey {
        /// set key for this addres
        bidder: HumanAddr,
        /// viewing key
        key: String,
    },
}
impl HandleCallback for SetKeyMsg {
    const BLOCK_SIZE: usize = BLOCK_SIZE;
}

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
    let versions: Vec<AuctionContractInfo> = vec![msg.auction_contract];
    let active: HashSet<Vec<u8>> = HashSet::new();
    let closed: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let symbols: Vec<String> = Vec::new();
    let symmap: HashMap<String, u16> = HashMap::new();
    let stopped = false;

    save(&mut deps.storage, VERSION_KEY, &versions)?;
    save(&mut deps.storage, ADMIN_KEY, &env.message.sender)?;
    save(&mut deps.storage, STATUS_KEY, &stopped)?;
    save(&mut deps.storage, PRNG_SEED_KEY, &prng_seed)?;
    save(&mut deps.storage, ACTIVE_KEY, &active)?;
    save(&mut deps.storage, CLOSED_KEY, &closed)?;
    save(&mut deps.storage, SYMBOLS_KEY, &symbols)?;
    save(&mut deps.storage, SYMBOLS_MAP_KEY, &symmap)?;

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
            description,
        } => try_create_auction(
            deps,
            env,
            label,
            sell_contract,
            bid_contract,
            sell_amount,
            minimum_bid,
            description,
        ),
        HandleMsg::RegisterAuction { seller, auction } => {
            try_register_auction(deps, env, &seller, &auction)
        }
        HandleMsg::RegisterBidder { bidder } => try_reg_bidder(deps, env, bidder),
        HandleMsg::RemoveBidder { bidder } => try_remove_bidder(deps, env, &bidder),
        HandleMsg::CloseAuction {
            seller,
            bidder,
            winning_bid,
        } => try_close_auction(deps, env, &seller, bidder.as_ref(), winning_bid),
        HandleMsg::CreateViewingKey { entropy } => try_create_key(deps, env, &entropy),
        HandleMsg::SetViewingKey { key, .. } => try_set_key(deps, env, &key),
        HandleMsg::NewAuctionContract { auction_contract } => {
            try_new_contract(deps, env, auction_contract)
        }
        HandleMsg::SetStatus { stop } => try_set_status(deps, env, stop),
    };
    pad_handle_result(response, BLOCK_SIZE)
}

#[allow(clippy::too_many_arguments)]
fn try_create_auction<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    label: String,
    sell_contract: ContractInfo,
    bid_contract: ContractInfo,
    sell_amount: Uint128,
    minimum_bid: Uint128,
    description: Option<String>,
) -> HandleResult {
    /// Instantiation message
    #[derive(Serialize)]
    pub struct AuctionInitMsg {
        /// factory contract code hash and address
        pub factory: ContractInfo,
        /// auction version number
        pub version: u8,
        /// String label for the auction
        pub label: String,
        /// sell symbol index
        pub sell_symbol: u16,
        /// bid symbol index
        pub bid_symbol: u16,
        /// auction seller
        pub seller: HumanAddr,
        /// sell contract code hash and address
        pub sell_contract: ContractInfo,
        /// bid contract code hash and address
        pub bid_contract: ContractInfo,
        /// amount of tokens being sold
        pub sell_amount: Uint128,
        /// minimum bid that will be accepted
        pub minimum_bid: Uint128,
        /// Optional free-form description of the auction (best to avoid double quotes). As an example
        /// it could be the date the owner will likely finalize the auction, or a list of other
        /// auctions for the same token, etc...
        #[serde(default)]
        pub description: Option<String>,
    }

    impl InitCallback for AuctionInitMsg {
        const BLOCK_SIZE: usize = BLOCK_SIZE;
    }

    let stop: bool = load(&deps.storage, STATUS_KEY)?;
    if stop {
        return Err(StdError::generic_err(
            "The factory has been stopped.  No new auctions can be created",
        ));
    }

    let factory = ContractInfo {
        code_hash: env.contract_code_hash,
        address: env.contract.address,
    };
    let mut symmap: HashMap<String, u16> = load(&deps.storage, SYMBOLS_MAP_KEY)?;
    // get sell token info
    let sell_token_info = token_info_query(
        &deps.querier,
        BLOCK_SIZE,
        sell_contract.code_hash.clone(),
        sell_contract.address.clone(),
    )?;
    let may_sell_index = symmap.get_mut(&sell_token_info.symbol).copied();
    // get bid token info
    let bid_token_info = token_info_query(
        &deps.querier,
        BLOCK_SIZE,
        bid_contract.code_hash.clone(),
        bid_contract.address.clone(),
    )?;
    let may_bid_index = symmap.get_mut(&bid_token_info.symbol).copied();
    let add_symbol = may_sell_index.is_none() || may_bid_index.is_none();
    let sell_index: u16;
    let bid_index: u16;
    // if there is a new symbol
    if add_symbol {
        let mut symbols: Vec<String> = load(&deps.storage, SYMBOLS_KEY)?;
        match may_sell_index {
            Some(unwrap) => sell_index = unwrap,
            None => {
                sell_index = symbols.len() as u16;
                symmap.insert(sell_token_info.symbol.clone(), sell_index);
                symbols.push(sell_token_info.symbol)
            }
        }
        match may_bid_index {
            Some(unwrap) => bid_index = unwrap,
            None => {
                bid_index = symbols.len() as u16;
                symmap.insert(bid_token_info.symbol.clone(), bid_index);
                symbols.push(bid_token_info.symbol)
            }
        }
        save(&mut deps.storage, SYMBOLS_KEY, &symbols)?;
        save(&mut deps.storage, SYMBOLS_MAP_KEY, &symmap)?;
    } else {
        sell_index = may_sell_index.unwrap();
        bid_index = may_bid_index.unwrap();
    }

    let mut versions: Vec<AuctionContractInfo> = load(&deps.storage, VERSION_KEY)?;
    let version = (versions.len() - 1) as u8;
    let auction_info = versions
        .pop()
        .ok_or_else(|| StdError::generic_err("Auction contract version history is corrupted"))?;
    let initmsg = AuctionInitMsg {
        factory,
        version,
        label: label.clone(),
        sell_symbol: sell_index,
        bid_symbol: bid_index,
        seller: env.message.sender,
        sell_contract,
        bid_contract,
        sell_amount,
        minimum_bid,
        description,
    };
    // save label and only register an auction giving the matching label
    save(&mut deps.storage, PENDING_KEY, &label)?;

    let cosmosmsg =
        initmsg.to_cosmos_msg(label, auction_info.code_id, auction_info.code_hash, None)?;

    Ok(HandleResponse {
        messages: vec![cosmosmsg],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::Status {
            status: Success,
            message: None,
        })?),
    })
}

fn try_register_auction<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    seller: &HumanAddr,
    auction: &StoreAuctionInfo,
) -> HandleResult {
    // verify this is the auction we are waiting for
    let load_label: Option<String> = may_load(&deps.storage, PENDING_KEY)?;
    let auth_label =
        load_label.ok_or_else(|| StdError::generic_err("Unable to authenticate registration."))?;
    if auth_label != auction.label {
        return Err(StdError::generic_err(
            "Label does not match the auction we are creating",
        ));
    }
    remove(&mut deps.storage, PENDING_KEY);

    // save the auction info keyed by its address
    let auction_addr = &deps.api.canonical_address(&env.message.sender)?;
    let mut info_store = PrefixedStorage::new(PREFIX_ACTIVE_INFO, &mut deps.storage);
    save(&mut info_store, auction_addr.as_slice(), auction)?;

    // add the auction address to list of active auctions
    let mut active: HashSet<Vec<u8>> = load(&deps.storage, ACTIVE_KEY)?;
    active.insert(auction_addr.as_slice().to_vec());
    save(&mut deps.storage, ACTIVE_KEY, &active)?;

    // get list of seller's active auctions
    let seller_raw = &deps.api.canonical_address(&seller)?;
    let mut seller_store = PrefixedStorage::new(PREFIX_SELLERS_ACTIVE, &mut deps.storage);
    let load_auctions: Option<HashSet<Vec<u8>>> = may_load(&seller_store, seller_raw.as_slice())?;
    let mut my_active = load_auctions.unwrap_or_default();
    // add this auction to seller's list
    my_active.insert(auction_addr.as_slice().to_vec());
    save(&mut seller_store, seller_raw.as_slice(), &my_active)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![log("address", env.message.sender)],
        data: None,
    })
}

fn try_close_auction<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    seller: &HumanAddr,
    bidder: Option<&HumanAddr>,
    winning_bid: Option<Uint128>,
) -> HandleResult {


//let mut tmplog = Vec::new();


    let auction_addr = &deps.api.canonical_address(&env.message.sender)?;

    // verify auction is in active list of auctions and not a spam attempt
    let mut authenticator = AuthResult::authenticate_auction(&deps.storage, auction_addr)?;

    let may_error = authenticator.error.take();
    if let Some(error) = may_error {
        return error;
    }

    let mut active = authenticator.active.take().unwrap();

    // remove the auction from the active list
    active.remove(&auction_addr.as_slice().to_vec());
    save(&mut deps.storage, ACTIVE_KEY, &active)?;

    // get the auction information
    let mut info_store = PrefixedStorage::new(PREFIX_ACTIVE_INFO, &mut deps.storage);
    let load_auction_info: Option<StoreAuctionInfo> =
        may_load(&info_store, auction_addr.as_slice())?;
    if load_auction_info.is_none() {
        return Ok(HandleResponse {
            messages: vec![],
            log: vec![
                log(
                    "Error",
                    "Unable to register the closure with the factory contract.",
                ),
                log("Reason", "Missing auction information."),
            ],
            data: None,
        });
    }
    // delete the active auction info
    info_store.remove(auction_addr.as_slice());

    // set the closed auction info
    let timestamp = env.block.time;
    let auction_info = load_auction_info.unwrap();
    let closed_info =
        auction_info.to_store_closed_auction_info(winning_bid.map(|n| n.u128()), timestamp);
    let mut closed_info_store = PrefixedStorage::new(PREFIX_CLOSED_INFO, &mut deps.storage);
    save(
        &mut closed_info_store,
        auction_addr.as_slice(),
        &closed_info,
    )?;

    // save auction to the closed list
    let may_closed: Option<BTreeMap<u64, Vec<u8>>> = may_load(&deps.storage, CLOSED_KEY)?;
    let mut closed = may_closed.unwrap_or_default();
    closed.insert(timestamp, auction_addr.as_slice().to_vec());
    save(&mut deps.storage, CLOSED_KEY, &closed)?;

    // remove auction from seller's active list
    let seller_raw = &deps.api.canonical_address(seller)?;
    remove_from_persons_active(
        &mut deps.storage,
        PREFIX_SELLERS_ACTIVE,
        seller_raw,
        auction_addr,
    )?;
    // add to seller's closed list
    add_to_persons_closed(
        &mut deps.storage,
        PREFIX_SELLERS_CLOSED,
        seller_raw,
        auction_addr,
    )?;

    // if auction had a winner
    if let Some(winner) = bidder {
        let winner_raw = &deps.api.canonical_address(winner)?;
        // clean up the bidders list of active auctions
        let mut dummy = false;
        let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
        let win_active = filter_only_active(&bidder_store, winner_raw, &mut active, &mut dummy)?;
        save(&mut bidder_store, winner_raw.as_slice(), &win_active)?;
        // add to winner's closed list
        add_to_persons_closed(&mut deps.storage, PREFIX_WINNERS, winner_raw, auction_addr)?;
    }

    Ok(HandleResponse {
        messages: vec![],
        ////////
        //        log: vec![],
        log: vec![
            log("auction", format!("{}", env.message.sender)),
            log("seller", format!("{}", seller)),
        ],
        data: None,
    })
}

fn try_reg_bidder<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    bidder: HumanAddr,
) -> HandleResult {
    let auction_addr = &deps.api.canonical_address(&env.message.sender)?;

    // verify auction is in active list of auctions and not a spam attempt
    let mut authenticator = AuthResult::authenticate_auction(&deps.storage, auction_addr)?;

    let may_error = authenticator.error.take();
    if let Some(error) = may_error {
        return error;
    }

    let mut active = authenticator.active.take().unwrap();

    // clean up the bidders list of active auctions
    let mut dummy = false;
    let bidder_raw = &deps.api.canonical_address(&bidder)?;
    let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
    let mut my_active = filter_only_active(&bidder_store, bidder_raw, &mut active, &mut dummy)?;
    // add this auction to the list
    my_active.insert(auction_addr.as_slice().to_vec());
    save(&mut bidder_store, bidder_raw.as_slice(), &my_active)?;

    // set viewing key (if exists) with this auction
    let read_key = ReadonlyPrefixedStorage::new(PREFIX_VIEW_KEY, &deps.storage);
    let load_key: Option<ViewingKey> = may_load(&read_key, bidder_raw.as_slice())?;

    let mut cosmos_msg = Vec::new();
    if let Some(my_key) = load_key {
        let load_versions: Option<Vec<AuctionContractInfo>> = may_load(&deps.storage, VERSION_KEY)?;
        if let Some(mut versions) = load_versions {
            let key: String = format!("{}", my_key);
            let read_infos = ReadonlyPrefixedStorage::new(PREFIX_ACTIVE_INFO, &deps.storage);
            let auction: Option<StoreAuctionInfo> =
                may_load(&read_infos, auction_addr.as_slice())?;
            if let Some(auction_info) = auction {
                if (auction_info.version as usize) < versions.len() {
                    //                let set_msg = SetKeyMsg::SetViewingKey { bidder, key };
                    let set_msg = SetKeyMsg::SetViewingKey {
                        bidder: bidder.clone(),
                        key,
                    };

                    let contract_info = versions.swap_remove(auction_info.version as usize);
                    cosmos_msg.push(set_msg.to_cosmos_msg(
                        contract_info.code_hash,
                        //                        env.message.sender,
                        env.message.sender.clone(),
                        None,
                    )?);
                } else {
                    return Ok(HandleResponse {
                        messages: vec![],
                        log: vec![
                            log(
                                "Error",
                                "Unable to register the bid with the factory contract.",
                            ),
                            log("Reason", "Missing auction contract info"),
                        ],
                        data: None,
                    });
                }
            } else {
                return Ok(HandleResponse {
                    messages: vec![],
                    log: vec![
                        log(
                            "Error",
                            "Unable to register the bid with the factory contract.",
                        ),
                        log("Reason", "Missing auction information"),
                    ],
                    data: None,
                });
            }
        } else {
            return Ok(HandleResponse {
                messages: vec![],
                log: vec![
                    log(
                        "Error",
                        "Unable to register the bid with the factory contract.",
                    ),
                    log("Reason", "Missing auction version list."),
                ],
                data: None,
            });
        }
    }

    Ok(HandleResponse {
        messages: cosmos_msg,
        ////////
        //        log: vec![],
        log: vec![
            log("auction", format!("{}", env.message.sender)),
            log("bidder", format!("{}", bidder)),
        ],
        data: None,
    })
}

fn try_remove_bidder<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    bidder: &HumanAddr,
) -> HandleResult {
    let auction_addr = &deps.api.canonical_address(&env.message.sender)?;
    // verify auction is in active list of auctions and not a spam attempt
    let mut authenticator = AuthResult::authenticate_auction(&deps.storage, auction_addr)?;

    let may_error = authenticator.error.take();
    if let Some(error) = may_error {
        return error;
    }

    let mut active = authenticator.active.take().unwrap();

    // clean up the bidders list of active auctions
    let mut dummy = false;
    let bidder_raw = &deps.api.canonical_address(&bidder)?;
    let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
    let mut my_active = filter_only_active(&bidder_store, bidder_raw, &mut active, &mut dummy)?;
    // remove this auction from the list
    my_active.remove(&auction_addr.as_slice().to_vec());
    save(&mut bidder_store, bidder_raw.as_slice(), &my_active)?;

    Ok(HandleResponse {
        /////////
        messages: vec![],
        log: vec![
            log("auction", format!("{}", env.message.sender)),
            log("bidder", format!("{}", bidder)),
        ],
        //        log: vec![],
        data: None,
    })
}

pub struct AuthResult {
    pub active: Option<HashSet<Vec<u8>>>,
    pub error: Option<HandleResult>,
}

impl AuthResult {
    pub fn authenticate_auction<S: Storage>(
        storage: &S,
        auction: &CanonicalAddr,
    ) -> StdResult<Self> {
        let error: Option<HandleResult>;
        let active: Option<HashSet<Vec<u8>>>;
        // verify auction is in active list of auctions and not a spam attempt
        let load_active: Option<HashSet<Vec<u8>>> = may_load(storage, ACTIVE_KEY)?;
        if let Some(active_set) = load_active {
            if !active_set.contains(&auction.as_slice().to_vec()) {
                error = Some(Ok(HandleResponse {
                    messages: vec![],
                    log: vec![log(
                        "Unauthorized",
                        "You are not an active auction this factory created.",
                    )],
                    data: None,
                }));
            } else {
                error = None;
            }
            active = Some(active_set);
        // if you can't load the active auction list, it is an error but still the auction process
        } else {
            error = Some(Ok(HandleResponse {
                messages: vec![],
                log: vec![
                    log(
                        "Error",
                        "Unable to register action with the factory contract.",
                    ),
                    log("Reason", "Missing active auction list."),
                ],
                data: None,
            }));
            active = None;
        }
        Ok(Self { active, error })
    }
}

fn try_new_contract<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    auction_contract: AuctionContractInfo,
) -> HandleResult {
    let admin: HumanAddr = load(&deps.storage, ADMIN_KEY)?;
    if env.message.sender != admin {
        return Err(StdError::generic_err(
            "This is an admin command. Admin commands can only be run from admin address",
        ));
    }
    let mut versions: Vec<AuctionContractInfo> = load(&deps.storage, VERSION_KEY)?;
    versions.push(auction_contract);
    save(&mut deps.storage, VERSION_KEY, &versions)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::Status {
            status: Success,
            message: None,
        })?),
    })
}

fn try_set_status<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    stop: bool,
) -> HandleResult {
    let admin: HumanAddr = load(&deps.storage, ADMIN_KEY)?;
    if env.message.sender != admin {
        return Err(StdError::generic_err(
            "This is an admin command. Admin commands can only be run from admin address",
        ));
    }
    save(&mut deps.storage, STATUS_KEY, &stop)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::Status {
            status: Success,
            message: None,
        })?),
    })
}

fn try_create_key<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    entropy: &str,
) -> HandleResult {
    let prng_seed: Vec<u8> = load(&deps.storage, PRNG_SEED_KEY)?;
    let key = ViewingKey::new(&env, &prng_seed, entropy.as_ref());
    let message_sender = &deps.api.canonical_address(&env.message.sender)?;
    let mut key_store = PrefixedStorage::new(PREFIX_VIEW_KEY, &mut deps.storage);
    save(&mut key_store, message_sender.as_slice(), &key)?;

    // clean up bidder's active list and set the viewing key with all active auctions
    let keystr = format!("{}", key);
    set_key_with_auctions(deps, &env.message.sender, message_sender, &keystr)
}

fn try_set_key<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    key: &str,
) -> HandleResult {
    let vk = ViewingKey(key.to_string());
    let message_sender = &deps.api.canonical_address(&env.message.sender)?;
    let mut key_store = PrefixedStorage::new(PREFIX_VIEW_KEY, &mut deps.storage);
    save(&mut key_store, message_sender.as_slice(), &vk)?;

    // clean up bidder's active list and set the viewing key with all active auctions
    set_key_with_auctions(deps, &env.message.sender, message_sender, &key)
}

fn set_key_with_auctions<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    msg_sender: &HumanAddr,
    sender_raw: &CanonicalAddr,
    keystr: &str,
) -> HandleResult {
let mut tmplog = Vec::new();

    // clean up the bidder's list of active auctions
    let load_active: Option<HashSet<Vec<u8>>> = may_load(&deps.storage, ACTIVE_KEY)?;
    // even if you can't update the active list, let the key get set
    if load_active.is_none() {
        return Ok(HandleResponse {
            messages: vec![],
            log: vec![],
            data: Some(to_binary(&HandleAnswer::Status {
                status: Success,
                message: Some("didn't load active".to_string()),

//                message: None,
            })?),
        });
    }
    let mut active = load_active.unwrap();
    let mut update = false;
    let mut bidder_store = PrefixedStorage::new(PREFIX_BIDDERS, &mut deps.storage);
    
    
    
    let load_auctions: Option<HashSet<Vec<u8>>> = may_load(&bidder_store, sender_raw.as_slice())?;
tmplog.push(log("loaded auctions",format!("{:?}",load_auctions)));



    let my_active = filter_only_active(&bidder_store, sender_raw, &mut active, &mut update)?;
    // if list was updated, save it
    if update {
        save(&mut bidder_store, sender_raw.as_slice(), &my_active)?;
    }
tmplog.push(log("my active count",format!("{}",my_active.len())));
    let mut cosmos_msg = Vec::new();
    // set viewing key with every active auction you have a bid with
    if !my_active.is_empty() {
        let load_versions: Option<Vec<AuctionContractInfo>> = may_load(&deps.storage, VERSION_KEY)?;
        if load_versions.is_none() {
            return Ok(HandleResponse {
                messages: vec![],
                log: vec![],
                data: Some(to_binary(&HandleAnswer::Status {
                    status: Success,
                    message: Some("However, unable to set viewing key with individual auctions.  Version list is missing.".to_string()),
                })?)
            });
        }
        let versions = load_versions.unwrap();
        let read_infos = ReadonlyPrefixedStorage::new(PREFIX_ACTIVE_INFO, &deps.storage);
        for addr in my_active {
            let load_auction: Option<StoreAuctionInfo> = may_load(&read_infos, addr.as_slice())?;
            if let Some(auction_info) = load_auction {
tmplog.push(log("loaded auction info",format!("{:?}",auction_info)));

                let get_contract_info = versions.get(auction_info.version as usize);
                if let Some(contract_info) = get_contract_info {
                    let set_msg = SetKeyMsg::SetViewingKey {
                        bidder: msg_sender.clone(),
                        key: keystr.to_string(),
                    };
                    let human = deps.api.human_address(&CanonicalAddr(Binary(addr)))?;
                    cosmos_msg.push(set_msg.to_cosmos_msg(
                        contract_info.code_hash.clone(),
                        human,
                        None,
                    )?);
                }
            }
        }
    }

    Ok(HandleResponse {
        messages: cosmos_msg,

        log: tmplog,

        data: Some(to_binary(&HandleAnswer::ViewingKey {
            key: keystr.to_string()})?)})
}

fn add_to_persons_closed<S: Storage>(
    storage: &mut S,
    prefix: &[u8],
    person: &CanonicalAddr,
    auction: &CanonicalAddr,
) -> StdResult<()> {
    let mut store = PrefixedStorage::new(prefix, storage);
    let load_closed: Option<Vec<Vec<u8>>> = may_load(&store, person.as_slice())?;
    let mut closed = load_closed.unwrap_or_default();
    closed.push(auction.as_slice().to_vec());
    save(&mut store, person.as_slice(), &closed)?;
    Ok(())
}

//fn add_to_persons_closed<S: Storage>(
//  storage: &S,
//prefix: &[u8],
//    person: &CanonicalAddr,
//  auction: &CanonicalAddr,
//) -> StdResult<()> {
//  let read = ReadonlyPrefixedStorage::new(prefix, &(*storage));
//let mut store = PrefixedStorage::new(prefix, &mut (*storage));
//    add_to_closed(&read, &mut store, person.as_slice(), auction)
//}

//fn add_to_closed<R: ReadonlyStorage, S: Storage>(
//  readonly: &R,
//storage: &mut S,
//    key: &[u8],
//  auction: &CanonicalAddr,
//) -> StdResult<()> {
//  let mut load_closed: Option<Vec<Vec<u8>>> = may_load(readonly, key)?;
//let mut closed = load_closed.unwrap_or_default();
//    closed.push(auction.as_slice().to_vec());
//  save(storage, key, &closed)?;
//Ok(())
//}

fn remove_from_persons_active<S: Storage>(
    storage: &mut S,
    prefix: &[u8],
    person: &CanonicalAddr,
    auction: &CanonicalAddr,
) -> StdResult<()> {
    let mut store = PrefixedStorage::new(prefix, storage);
    let load_active: Option<HashSet<Vec<u8>>> = may_load(&store, person.as_slice())?;
    if let Some(mut active) = load_active {
        active.remove(&auction.as_slice().to_vec());
        save(&mut store, person.as_slice(), &active)?;
    }
    Ok(())
}

#[allow(clippy::needless_return)]
fn filter_only_active<S: Storage>(
    storage: &S,
    address: &CanonicalAddr,
    active: &mut HashSet<Vec<u8>>,
    updated: &mut bool,
) -> StdResult<HashSet<Vec<u8>>> {
    //    let read_list = ReadonlyPrefixedStorage::new(PREFIX_BIDDERS, &(*storage));
    let load_auctions: Option<HashSet<Vec<u8>>> = may_load(storage, address.as_slice())?;

    if let Some(my_auctions) = load_auctions {
        let start_len = my_auctions.len();
        let my_active: HashSet<Vec<u8>> =
            my_auctions.iter().filter_map(|v| active.take(v)).collect();
        *updated = start_len != my_active.len();
        return Ok(my_active);
    } else {
        *updated = false;
        Ok(HashSet::new())
    }
}

pub fn query<S: Storage, A: Api, Q: Querier>(deps: &Extern<S, A, Q>, msg: QueryMsg) -> QueryResult {
        let response = match msg {
            QueryMsg::ListMyAuctions { address, viewing_key, filter } => try_list_my(deps, &address, viewing_key, filter),
            QueryMsg::ListActiveAuctions { } => try_list_active(deps),
            QueryMsg::ListClosedAuctions { before, page_size } => try_list_closed(deps, before, page_size),
        };
        pad_query_result(response, BLOCK_SIZE)
}

fn try_list_closed<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    before: Option<u64>,
    page_size: Option<u32>,
) -> QueryResult {
    to_binary(&QueryAnswer::ListClosedAuctions{
        closed: display_closed(&deps.api, &deps.storage, before, page_size)?,
    })
}

fn try_list_active<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
) -> QueryResult {
    to_binary(&QueryAnswer::ListActiveAuctions{
        active: display_active_list(&deps.api, &deps.storage, None, ACTIVE_KEY)?,
    })
}

fn try_list_my<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    address: &HumanAddr,
    viewing_key: String,
    filter: Option<FilterTypes>,
) -> QueryResult {
    let addr_raw = &deps.api.canonical_address(address)?;
    let read_key = ReadonlyPrefixedStorage::new(PREFIX_VIEW_KEY, &deps.storage);
    let load_key: Option<ViewingKey> = may_load(&read_key, addr_raw.as_slice())?;

    let input_key = ViewingKey(viewing_key);
    let expected_key = load_key.unwrap_or(ViewingKey("Still take time to check if none".to_string()));

    // if it matches
        if input_key.check_viewing_key(&expected_key.to_hashed()) {
            let mut active_lists: Option<MyActiveLists> = None;
            let mut closed_lists: Option<MyClosedLists> = None;
            let types = filter.unwrap_or(FilterTypes::All);

            if types == FilterTypes::Active || types == FilterTypes::All {
                let seller_active = display_active_list(&deps.api, &deps.storage, Some(PREFIX_SELLERS_ACTIVE), addr_raw.as_slice())?;
                let bidder_active = display_active_list(&deps.api, &deps.storage, Some(PREFIX_BIDDERS), addr_raw.as_slice())?;
                if seller_active.is_some() || bidder_active.is_some() {
                    active_lists = Some(MyActiveLists{as_seller: seller_active, as_bidder: bidder_active});
                }
            }
            if types == FilterTypes::Closed || types == FilterTypes::All {
                let seller_closed = display_addr_closed(&deps.api, &deps.storage, PREFIX_SELLERS_CLOSED, addr_raw.as_slice())?;
                let won = display_addr_closed(&deps.api, &deps.storage, PREFIX_WINNERS, addr_raw.as_slice())?;
                if seller_closed.is_some() || won.is_some() {
                    closed_lists = Some(MyClosedLists{as_seller: seller_closed, won});
                }
            }
         
            return to_binary(&QueryAnswer::ListMyAuctions {
                active: active_lists,
                closed: closed_lists,
            });
        } else {
 

    to_binary(&QueryAnswer::ViewingKeyError {
        error: "Wrong viewing key for this address or viewing key not set".to_string(),
    })
}
}

fn display_active_list<S: ReadonlyStorage, A: Api>(
    api: &A,
    storage: &S,
    prefix: Option<&[u8]>,
    key: &[u8],
) -> StdResult<Option<Vec<AuctionInfo>>> {
    let load_list: Option<HashSet<Vec<u8>>> =
    if let Some(pref) = prefix {
        let read = &ReadonlyPrefixedStorage::new(pref, storage);
        may_load(read, key)?
    } else {
        may_load(storage, key)?
    };
    let mut actives = match load_list {
        Some(list) => {
            let mut display_list = Vec::new();
            let read_info = &ReadonlyPrefixedStorage::new(PREFIX_ACTIVE_INFO, storage);
            // get the token symbol strings
            let symbols: Vec<String> = load(storage, SYMBOLS_KEY)?;
            for addr in list.iter() {
                // get this auction's info
                let load_info: Option<StoreAuctionInfo> = may_load(read_info, addr.as_slice())?;
                if let Some(info) = load_info {
                    let may_sell_sym = symbols.get(info.sell_symbol as usize);
                    if let Some(sell_sym) = may_sell_sym {
                        let may_bid_sym = symbols.get(info.bid_symbol as usize);
                        if let Some(bid_sym) = may_bid_sym {
                            let pair = format!("{}-{}",sell_sym, bid_sym);
                            display_list.push(AuctionInfo{address: api.human_address(&CanonicalAddr::from(addr.as_slice()))?, label: info.label, pair});
                        }
                    }
                }
            }
            display_list
        },
        None => Vec::new()
    };
    actives.sort_by(|a, b| { a.pair.cmp(&b.pair)});
    if actives.is_empty() {
        Ok(None)
    } else {
        Ok(Some(actives))
    }
}

fn display_addr_closed<S: ReadonlyStorage, A: Api>(
    api: &A,
    storage: &S,
    prefix: &[u8],
    key: &[u8],
) -> StdResult<Option<Vec<ClosedAuctionInfo>>> {
    let read_closed = &ReadonlyPrefixedStorage::new(prefix, storage);
    let load_list: Option<Vec<Vec<u8>>>  = may_load(read_closed, key)?;
    let closed = match load_list {
        Some(list) => {
            let mut display_list = Vec::new();
            let read_info = &ReadonlyPrefixedStorage::new(PREFIX_CLOSED_INFO, storage);
            // get token symbol strings
            let symbols: Vec<String> = load(storage, SYMBOLS_KEY)?;
            for addr in list.iter().rev() {
                // get this auction's info
                let load_info: Option<StoreClosedAuctionInfo> = may_load(read_info, addr.as_slice())?;
                if let Some(info) = load_info {
                    let may_sell_sym = symbols.get(info.sell_symbol as usize);
                    if let Some(sell_sym) = may_sell_sym {
                        let may_bid_sym = symbols.get(info.bid_symbol as usize);
                        if let Some(bid_sym) = may_bid_sym {
                            let pair = format!("{}-{}",sell_sym, bid_sym);
                            display_list.push(ClosedAuctionInfo{address: api.human_address(&CanonicalAddr::from(addr.as_slice()))?, label: info.label, pair, winning_bid: info.winning_bid.map(|n| Uint128(n)), timestamp: info.timestamp});
                        }
                    }
                }
            }
            display_list
        },
        None => Vec::new()
    };
    if closed.is_empty(){
        Ok(None)
    } else {
        Ok(Some(closed))
    }
}

fn display_closed<S: ReadonlyStorage, A: Api>(
    api: &A,
    storage: &S,
    start: Option<u64>,
    page_size: Option<u32>,
) -> StdResult<Option<Vec<ClosedAuctionInfo>>> {
    let load_list: Option<BTreeMap<u64, Vec<u8>>>  = may_load(storage, CLOSED_KEY)?;
    let closed = match load_list {
        Some(mut list) => {
            let mut display_list = Vec::new();
            let read_info = &ReadonlyPrefixedStorage::new(PREFIX_CLOSED_INFO, storage);
            // get the token symbol strings
            let symbols: Vec<String> = load(storage, SYMBOLS_KEY)?;
            // get the timestamp of the latest close
            if let Some(max) = list.keys().next_back() {
                // start iterating from the last close or before given start point
                let begin = start.unwrap_or(max + 1);
                let quant = page_size.unwrap_or(200) as usize;
                // grab the first 200 backwards from the starting point
                for (_k, addr) in list.range_mut(..begin).rev().take(quant) {
                    let load_info: Option<StoreClosedAuctionInfo> = may_load(read_info, addr.as_slice())?;
                    if let Some(info) = load_info {
                        let may_sell_sym = symbols.get(info.sell_symbol as usize);
                        if let Some(sell_sym) = may_sell_sym {
                            let may_bid_sym = symbols.get(info.bid_symbol as usize);
                            if let Some(bid_sym) = may_bid_sym {
                                let pair = format!("{}-{}",sell_sym, bid_sym);
                                display_list.push(ClosedAuctionInfo{address: api.human_address(&CanonicalAddr::from(addr.as_slice()))?, label: info.label, pair, winning_bid: info.winning_bid.map(|n| Uint128(n)), timestamp: info.timestamp});
                            }
                        }
                    }
                }
            }
            display_list
        },
        None => Vec::new()
    };
    if closed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(closed))
    }
}