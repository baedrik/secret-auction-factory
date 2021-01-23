use serde::{Deserialize, Serialize};

use cosmwasm_std::{
    log, to_binary, Api, CanonicalAddr, Env, Extern, HandleResponse, HandleResult, HumanAddr,
    InitResponse, InitResult, Querier, QueryResult, StdError, Storage, Uint128,
};

use std::collections::HashSet;

use serde_json_wasm as serde_json;

use secret_toolkit::utils::{pad_handle_result, pad_query_result, HandleCallback, Query};

use crate::msg::{
    ContractInfo, HandleAnswer, HandleMsg, InitMsg, QueryAnswer, QueryMsg, ResponseStatus,
    ResponseStatus::{Failure, Success},
    Token,
};
use crate::state::{load, may_load, remove, save, Bid, State};

use chrono::NaiveDateTime;

/// storage key for auction state
pub const CONFIG_KEY: &[u8] = b"config";

/// pad handle responses and log attributes to blocks of 256 bytes to prevent leaking info based on
/// response size
pub const BLOCK_SIZE: usize = 256;

/// auction info needed by factory
#[derive(Serialize)]
pub struct FactoryAuctionInfo {
    /// auction index with the factory
    pub index: u32,
    /// auction label
    pub label: String,
    /// sell symbol index
    pub sell_symbol: u16,
    /// bid symbol index
    pub bid_symbol: u16,
    /// sell amount
    pub sell_amount: Uint128,
    /// minimum bid
    pub minimum_bid: Uint128,
    /// timestamp after which anyone may close the auction
    /// Timestamp is in seconds since epoch 01/01/1970
    pub ends_at: u64,
}

/// the factory's handle messages this auction will call
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryHandleMsg {
    /// RegisterAuction saves the auction info of a newly instantiated auction
    RegisterAuction {
        /// address of the seller
        seller: HumanAddr,
        /// this auction's info
        auction: FactoryAuctionInfo,
        /// sell token contract info
        sell_contract: ContractInfo,
    },
    /// registers the closure of this auction with the factory
    CloseAuction {
        /// auction index
        index: u32,
        /// auction seller
        seller: HumanAddr,
        /// winning bidder if the auction ended in a swap
        bidder: Option<HumanAddr>,
        /// winning bid if the auction ended in a swap
        winning_bid: Option<Uint128>,
    },
    /// registers a new bidder with the factory
    RegisterBidder {
        /// auction index
        index: u32,
        /// bidder's address
        bidder: HumanAddr,
    },
    /// tells factory the address is no longer a bidder in this auction
    RemoveBidder {
        /// auction index
        index: u32,
        /// bidder's address
        bidder: HumanAddr,
    },
    /// tells factory the closing time and/or minimum bid changed
    ChangeAuctionInfo {
        /// auction index
        index: u32,
        /// optional new ends_at time in seconds since epoch 01/01/1970
        ends_at: Option<u64>,
        /// optional new minimum bid
        minimum_bid: Option<Uint128>,
    },
}

impl HandleCallback for FactoryHandleMsg {
    const BLOCK_SIZE: usize = BLOCK_SIZE;
}

/// the factory's query messages this auction will call
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryQueryMsg {
    /// authenticates the supplied address/viewing key.  This should only be called by auctions
    IsKeyValid {
        /// address whose viewing key is being authenticated
        address: HumanAddr,
        /// viewing key
        viewing_key: String,
    },
}

impl Query for FactoryQueryMsg {
    const BLOCK_SIZE: usize = BLOCK_SIZE;
}

/// result of authenticating address/key pair
#[derive(Serialize, Deserialize, Debug)]
pub struct IsKeyValid {
    pub is_valid: bool,
}

/// IsKeyValid wrapper struct
#[derive(Serialize, Deserialize, Debug)]
pub struct IsKeyValidWrapper {
    pub is_key_valid: IsKeyValid,
}

////////////////////////////////////// Init ///////////////////////////////////////
/// Returns InitResult
///
/// Initializes the auction state and registers Receive function with sell and bid
/// token contracts
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
    if msg.sell_amount == Uint128(0) {
        return Err(StdError::generic_err("Sell amount must be greater than 0"));
    }
    if msg.sell_contract.address == msg.bid_contract.address {
        return Err(StdError::generic_err(
            "Sell contract and bid contract must be different",
        ));
    }
    let state = State {
        factory: msg.factory.clone(),
        index: msg.index,
        auction_addr: env.contract.address,
        seller: msg.seller.clone(),
        sell_contract: msg.sell_contract.clone(),
        sell_decimals: msg.sell_decimals,
        bid_contract: msg.bid_contract,
        bid_decimals: msg.bid_decimals,
        sell_amount: msg.sell_amount.u128(),
        minimum_bid: msg.minimum_bid.u128(),
        currently_consigned: 0,
        bidders: HashSet::new(),
        ends_at: msg.ends_at,
        is_completed: false,
        tokens_consigned: false,
        description: msg.description,
        winning_bid: 0,
    };

    save(&mut deps.storage, CONFIG_KEY, &state)?;

    let auction = FactoryAuctionInfo {
        label: msg.label,
        index: msg.index,
        sell_symbol: msg.sell_symbol,
        bid_symbol: msg.bid_symbol,
        sell_amount: msg.sell_amount,
        minimum_bid: msg.minimum_bid,
        ends_at: msg.ends_at,
    };

    let reg_auction_msg = FactoryHandleMsg::RegisterAuction {
        seller: msg.seller,
        auction,
        sell_contract: msg.sell_contract,
    };
    // perform factory register callback
    let cosmos_msg =
        reg_auction_msg.to_cosmos_msg(msg.factory.code_hash, msg.factory.address, None)?;
    // and register receive with the bid/sell token contracts
    Ok(InitResponse {
        messages: vec![
            state
                .sell_contract
                .register_receive_msg(env.contract_code_hash.clone())?,
            state
                .bid_contract
                .register_receive_msg(env.contract_code_hash)?,
            cosmos_msg,
        ],
        log: vec![],
    })
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
        HandleMsg::RetractBid { .. } => try_retract(deps, env.message.sender),
        HandleMsg::Finalize {
            new_ends_at,
            new_minimum_bid,
        } => try_finalize(deps, env, new_ends_at, new_minimum_bid, false),
        HandleMsg::ReturnAll { .. } => try_finalize(deps, env, None, None, true),
        HandleMsg::Receive { from, amount, .. } => try_receive(deps, env, from, amount),
        HandleMsg::ChangeMinimumBid { minimum_bid } => try_change_min_bid(deps, env, minimum_bid),
    };
    pad_handle_result(response, BLOCK_SIZE)
}

/// Returns HandleResult
///
/// allows seller to change the minimum bid
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `minimum_bid` - new minimum bid
fn try_change_min_bid<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    minimum_bid: Uint128,
) -> HandleResult {
    let mut state: State = load(&deps.storage, CONFIG_KEY)?;
    // only allow the seller to change the minimum bid
    if env.message.sender != state.seller {
        return Err(StdError::generic_err(
            "Only the auction seller can change the minimum bid",
        ));
    }
    // no reason to change the min bid if the auction is over
    if state.is_completed {
        return Err(StdError::generic_err(
            "Can not change the minimum bid of an auction that has ended",
        ));
    }
    // save the min bid change
    state.minimum_bid = minimum_bid.u128();
    save(&mut deps.storage, CONFIG_KEY, &state)?;
    // register change with factory
    let change_min_msg = FactoryHandleMsg::ChangeAuctionInfo {
        index: state.index,
        ends_at: None,
        minimum_bid: Some(minimum_bid),
    };
    // perform factory callback
    let cosmos_msg =
        change_min_msg.to_cosmos_msg(state.factory.code_hash, state.factory.address, None)?;

    Ok(HandleResponse {
        messages: vec![cosmos_msg],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::ChangeMinimumBid {
            status: Success,
            minimum_bid,
            bid_decimals: state.bid_decimals,
        })?),
    })
}

/// Returns HandleResult
///
/// process the Receive message sent after either bid or sell token contract sent tokens to
/// auction escrow
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `from` - address of owner of tokens sent to escrow
/// * `amount` - Uint128 amount sent to escrow
fn try_receive<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    from: HumanAddr,
    amount: Uint128,
) -> HandleResult {
    let mut state: State = load(&deps.storage, CONFIG_KEY)?;

    if env.message.sender == state.sell_contract.address {
        try_consign(deps, from, amount, &mut state)
    } else if env.message.sender == state.bid_contract.address {
        try_bid(deps, env, from, amount, &mut state)
    } else {
        let message = format!(
            "Address: {} is not a token in this auction",
            env.message.sender
        );
        Err(StdError::generic_err(message))
    }
}

/// Returns HandleResult
///
/// process the attempt to consign sale tokens to auction escrow
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `owner` - address of owner of tokens sent to escrow
/// * `amount` - Uint128 amount sent to escrow
/// * `state` - mutable reference to the state of the auction
fn try_consign<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    owner: HumanAddr,
    amount: Uint128,
    state: &mut State,
) -> HandleResult {
    // if not the auction owner, send the tokens back
    if owner != state.seller {
        return Err(StdError::generic_err(
            "Only auction creator can consign tokens for sale.  Your tokens have been returned",
        ));
    }
    // if auction is over, send the tokens back
    if state.is_completed {
        return Err(StdError::generic_err(
            "Auction has ended. Your tokens have been returned",
        ));
    }
    // if tokens to be sold have already been consigned, return these tokens
    if state.tokens_consigned {
        return Err(StdError::generic_err(
            "Tokens to be sold have already been consigned. Your tokens have been returned",
        ));
    }

    let consign_total = state.currently_consigned + amount.u128();
    let mut log_msg = String::new();
    let mut cos_msg = Vec::new();
    let status: ResponseStatus;
    let mut excess: Option<Uint128> = None;
    let mut needed: Option<Uint128> = None;
    // if consignment amount < auction sell amount, ask for remaining balance
    if consign_total < state.sell_amount {
        state.currently_consigned = consign_total;
        needed = Some(Uint128(state.sell_amount - consign_total));
        status = Failure;
        log_msg.push_str(
            "You have not consigned the full amount to be sold.  You need to consign additional \
             tokens",
        );
    // all tokens to be sold have been consigned
    } else {
        state.tokens_consigned = true;
        state.currently_consigned = state.sell_amount;
        status = Success;
        log_msg.push_str("Tokens to be sold have been consigned to the auction");
        // if consigned more than needed, return excess tokens
        if consign_total > state.sell_amount {
            excess = Some(Uint128(consign_total - state.sell_amount));
            cos_msg.push(state.sell_contract.transfer_msg(owner, excess.unwrap())?);
            log_msg.push_str(".  Excess tokens have been returned");
        }
    }

    save(&mut deps.storage, CONFIG_KEY, &state)?;

    let resp = serde_json::to_string(&HandleAnswer::Consign {
        status,
        message: log_msg,
        amount_consigned: Uint128(state.currently_consigned),
        amount_needed: needed,
        amount_returned: excess,
        sell_decimals: state.sell_decimals,
    })
    .unwrap();

    Ok(HandleResponse {
        messages: cos_msg,
        log: vec![log("response", resp)],
        data: None,
    })
}

/// Returns HandleResult
///
/// process the bid attempt
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `bidder` - address of owner of tokens sent to escrow
/// * `amount` - Uint128 amount sent to escrow
/// * `state` - mutable reference to auction state
fn try_bid<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    bidder: HumanAddr,
    amount: Uint128,
    state: &mut State,
) -> HandleResult {
    // if auction is over, send the tokens back
    if state.is_completed {
        return Err(StdError::generic_err(
            "Auction has ended. Bid tokens have been returned",
        ));
    }
    // don't accept a 0 bid
    if amount == Uint128(0) {
        return Err(StdError::generic_err("Bid must be greater than 0"));
    }
    // if bid is less than the minimum accepted bid, send the tokens back
    if amount.u128() < state.minimum_bid {
        let message =
            String::from("Bid was less than minimum allowed.  Bid tokens have been returned");

        let resp = serde_json::to_string(&HandleAnswer::Bid {
            status: Failure,
            message,
            previous_bid: None,
            minimum_bid: Some(Uint128(state.minimum_bid)),
            amount_bid: None,
            amount_returned: Some(amount),
            bid_decimals: state.bid_decimals,
        })
        .unwrap();

        return Ok(HandleResponse {
            messages: vec![state.bid_contract.transfer_msg(bidder, amount)?],
            log: vec![log("response", resp)],
            data: None,
        });
    }
    let mut return_amount: Option<Uint128> = None;
    let bidder_raw = &deps.api.canonical_address(&bidder)?;
    let mut cosmos_msg = Vec::new();

    // if there is an active bid from this address
    if state.bidders.contains(&bidder_raw.as_slice().to_vec()) {
        let bid: Option<Bid> = may_load(&deps.storage, bidder_raw.as_slice())?;
        if let Some(old_bid) = bid {
            // if new bid is == the old bid, keep old bid and return this one
            if amount.u128() == old_bid.amount {
                let message = String::from(
                    "New bid is the same as previous bid.  Retaining previous timestamp",
                );

                let resp = serde_json::to_string(&HandleAnswer::Bid {
                    status: Failure,
                    message,
                    previous_bid: Some(amount),
                    minimum_bid: None,
                    amount_bid: Some(amount),
                    amount_returned: Some(amount),
                    bid_decimals: state.bid_decimals,
                })
                .unwrap();

                return Ok(HandleResponse {
                    messages: vec![state.bid_contract.transfer_msg(bidder, amount)?],
                    log: vec![log("response", resp)],
                    data: None,
                });
            // new bid is different, save the new bid, and return the old one, so mark for return
            } else {
                return_amount = Some(Uint128(old_bid.amount));
            }
        }
    // address did not have an active bid
    } else {
        // insert in list of bidders and save
        state.bidders.insert(bidder_raw.as_slice().to_vec());
        save(&mut deps.storage, CONFIG_KEY, &state)?;
        // register new bidder with the factory
        let reg_bid_msg = FactoryHandleMsg::RegisterBidder {
            index: state.index,
            bidder: bidder.clone(),
        };
        // perform register bidder callback
        cosmos_msg.push(reg_bid_msg.to_cosmos_msg(
            state.factory.code_hash.clone(),
            state.factory.address.clone(),
            None,
        )?);
    }
    let new_bid = Bid {
        amount: amount.u128(),
        timestamp: env.block.time,
    };
    save(&mut deps.storage, bidder_raw.as_slice(), &new_bid)?;

    let mut message = String::from("Bid accepted");

    // if need to return the old bid
    if let Some(returned) = return_amount {
        cosmos_msg.push(state.bid_contract.transfer_msg(bidder, returned)?);
        message.push_str(". Previously bid tokens have been returned");
    }
    let resp = serde_json::to_string(&HandleAnswer::Bid {
        status: Success,
        message,
        previous_bid: None,
        minimum_bid: None,
        amount_bid: Some(amount),
        amount_returned: return_amount,
        bid_decimals: state.bid_decimals,
    })
    .unwrap();

    Ok(HandleResponse {
        messages: cosmos_msg,
        log: vec![log("response", resp)],
        data: None,
    })
}

/// Returns HandleResult
///
/// attempt to retract current bid
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `bidder` - address of bidder
fn try_retract<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    bidder: HumanAddr,
) -> HandleResult {
    let mut state: State = load(&deps.storage, CONFIG_KEY)?;

    let bidder_raw = &deps.api.canonical_address(&bidder)?;
    let mut cos_msg = Vec::new();
    let sent: Option<Uint128>;
    let mut log_msg = String::new();
    let status: ResponseStatus;
    let bid_decimals = state.bid_decimals;
    // if there was a active bid from this address, remove the bid and return tokens
    if state.bidders.contains(&bidder_raw.as_slice().to_vec()) {
        let bid: Option<Bid> = may_load(&deps.storage, bidder_raw.as_slice())?;
        if let Some(old_bid) = bid {
            remove(&mut deps.storage, bidder_raw.as_slice());
            state.bidders.remove(&bidder_raw.as_slice().to_vec());
            save(&mut deps.storage, CONFIG_KEY, &state)?;
            cos_msg.push(
                state
                    .bid_contract
                    .transfer_msg(bidder.clone(), Uint128(old_bid.amount))?,
            );
            status = Success;
            sent = Some(Uint128(old_bid.amount));
            log_msg.push_str("Bid retracted.  Tokens have been returned");

            // let factory know bid was retracted
            let rem_bid_msg = FactoryHandleMsg::RemoveBidder {
                index: state.index,
                bidder,
            };
            // perform callback
            cos_msg.push(rem_bid_msg.to_cosmos_msg(
                state.factory.code_hash,
                state.factory.address,
                None,
            )?);
        } else {
            status = Failure;
            sent = None;
            log_msg.push_str(&format!("No active bid for address: {}", bidder));
        }
    // no active bid found
    } else {
        status = Failure;
        sent = None;
        log_msg.push_str(&format!("No active bid for address: {}", bidder));
    }
    Ok(HandleResponse {
        messages: cos_msg,
        log: vec![],
        data: Some(to_binary(&HandleAnswer::RetractBid {
            status,
            message: log_msg,
            amount_returned: sent,
            bid_decimals: sent.map(|_a| bid_decimals),
        })?),
    })
}

/// Returns HandleResult
///
/// closes the auction and sends all the tokens in escrow to where they belong
///
/// # Arguments
///
/// * `deps` - mutable reference to Extern containing all the contract's external dependencies
/// * `env` - Env of contract's environment
/// * `new_ends_at` - optional epoch timestamp to extend closing time to if there are no bids
/// * `new_minimum_bid` - optional minimum bid update if there are no bids
/// * `return_all` - true if being called from the return_all fallback plan
fn try_finalize<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    new_ends_at: Option<u64>,
    new_minimum_bid: Option<Uint128>,
    return_all: bool,
) -> HandleResult {
    let mut state: State = load(&deps.storage, CONFIG_KEY)?;

    // can only do a return_all if the auction is closed
    if return_all && !state.is_completed {
        return Err(StdError::generic_err(
            "return_all can only be executed after the auction has ended",
        ));
    }
    let is_seller = env.message.sender == state.seller;
    let update_ends_at = new_ends_at.is_some();
    let update_min_bid = new_minimum_bid.is_some();
    // can not change minimum bid or closing time if not the owner
    if !is_seller && (update_ends_at || update_min_bid) {
        return Err(StdError::generic_err(
            "Only the auction seller can change the closing time or the minimum bid",
        ));
    }
    // if not the auction owner, can't finalize before the closing time, but you can return_all
    if !return_all && !is_seller && (env.block.time < state.ends_at) {
        return Err(StdError::generic_err(
            "Only auction creator can finalize the sale before the closing time",
        ));
    }
    let no_bids = state.bidders.is_empty();
    // if there are no active bids, and closer wants to extend the auction
    if no_bids && !state.is_completed && (update_ends_at || update_min_bid) {
        if let Some(ends_at) = new_ends_at {
            state.ends_at = ends_at;
        }
        if let Some(minimum_bid) = new_minimum_bid {
            state.minimum_bid = minimum_bid.u128();
        }
        save(&mut deps.storage, CONFIG_KEY, &state)?;
        // register change with factory
        let change_min_msg = FactoryHandleMsg::ChangeAuctionInfo {
            index: state.index,
            ends_at: new_ends_at,
            minimum_bid: new_minimum_bid,
        };
        // perform factory callback
        let factory_msg =
            change_min_msg.to_cosmos_msg(state.factory.code_hash, state.factory.address, None)?;
        let time_str = if update_ends_at { " closing time" } else { "" };
        let bid_str = if update_min_bid { " minimum bid" } else { "" };
        let and_str = if update_ends_at && update_min_bid {
            " and"
        } else {
            ""
        };
        let message = format!(
            "There were no active bids.  The{}{}{} has been updated",
            time_str, and_str, bid_str
        );
        return Ok(HandleResponse {
            messages: vec![factory_msg],
            log: vec![],
            data: Some(to_binary(&HandleAnswer::CloseAuction {
                status: Failure,
                message,
                winning_bid: None,
                bid_decimals: None,
                sell_tokens_received: None,
                sell_decimals: None,
                bid_tokens_received: None,
            })?),
        });
    }
    let mut cos_msg = Vec::new();
    let mut update_state = false;
    let mut winning_amount: Option<Uint128> = None;
    let mut bid_decimals: Option<u8> = None;
    let mut winner: Option<HumanAddr> = None;
    let mut sell_tokens_received: Option<Uint128> = None;
    let mut sell_decimals: Option<u8> = None;
    let mut bid_tokens_received: Option<Uint128> = None;
    let mut is_winner = false;
    let mut is_loser = false;

    // if there were bids
    if !no_bids {
        // load all the bids
        struct OwnedBid {
            pub bidder: CanonicalAddr,
            pub bid: Bid,
        }
        let mut bid_list: Vec<OwnedBid> = Vec::new();
        for bidder in &state.bidders {
            let bid: Option<Bid> = may_load(&deps.storage, bidder.as_slice())?;
            if let Some(found_bid) = bid {
                bid_list.push(OwnedBid {
                    bidder: CanonicalAddr::from(bidder.as_slice()),
                    bid: found_bid,
                });
            }
        }
        // closing an auction that has been fully consigned
        if state.tokens_consigned && !state.is_completed {
            bid_list.sort_by(|a, b| {
                a.bid
                    .amount
                    .cmp(&b.bid.amount)
                    .then(b.bid.timestamp.cmp(&a.bid.timestamp))
            });
            // if there was a winner, swap the tokens
            if let Some(winning_bid) = bid_list.pop() {
                cos_msg.push(
                    state
                        .bid_contract
                        .transfer_msg(state.seller.clone(), Uint128(winning_bid.bid.amount))?,
                );
                let human_winner = deps.api.human_address(&winning_bid.bidder)?;
                cos_msg.push(
                    state
                        .sell_contract
                        .transfer_msg(human_winner.clone(), Uint128(state.sell_amount))?,
                );
                winning_amount = Some(Uint128(winning_bid.bid.amount));
                if is_seller {
                    bid_tokens_received = winning_amount;
                }
                if human_winner == env.message.sender {
                    is_winner = true;
                    sell_tokens_received = Some(Uint128(state.sell_amount));
                    sell_decimals = Some(state.sell_decimals);
                }
                state.currently_consigned = 0;
                update_state = true;
                winner = Some(human_winner);
                state.winning_bid = winning_bid.bid.amount;
                remove(&mut deps.storage, &winning_bid.bidder.as_slice());
                state
                    .bidders
                    .remove(&winning_bid.bidder.as_slice().to_vec());
            }
        }
        // loops through all remaining bids to return them to the bidders
        for losing_bid in &bid_list {
            let human_loser = deps.api.human_address(&losing_bid.bidder)?;
            if human_loser == env.message.sender {
                is_loser = true;
                // if the seller also placed a losing bid, add them
                bid_tokens_received = Some(
                    bid_tokens_received.unwrap_or(Uint128(0)) + Uint128(losing_bid.bid.amount),
                );
                bid_decimals = Some(state.bid_decimals);
            }
            cos_msg.push(
                state
                    .bid_contract
                    .transfer_msg(human_loser, Uint128(losing_bid.bid.amount))?,
            );
            remove(&mut deps.storage, &losing_bid.bidder.as_slice());
            update_state = true;
            state.bidders.remove(&losing_bid.bidder.as_slice().to_vec());
        }
    }
    // return any tokens that have been consigned to the auction owner (can happen if owner
    // finalized the auction before consigning the full sale amount or if there were no bids)
    if state.currently_consigned > 0 {
        cos_msg.push(
            state
                .sell_contract
                .transfer_msg(state.seller.clone(), Uint128(state.currently_consigned))?,
        );
        if is_seller {
            sell_tokens_received = Some(Uint128(state.currently_consigned));
            sell_decimals = Some(state.sell_decimals);
        }
        state.currently_consigned = 0;
        update_state = true;
    }
    // mark that auction had ended
    if !state.is_completed {
        state.is_completed = true;
        update_state = true;
        // let factory know
        let close_msg = FactoryHandleMsg::CloseAuction {
            index: state.index,
            seller: state.seller.clone(),
            bidder: winner,
            winning_bid: winning_amount,
        }
        .to_cosmos_msg(
            state.factory.code_hash.clone(),
            state.factory.address.clone(),
            None,
        )?;
        cos_msg.push(close_msg);
    }
    if update_state {
        save(&mut deps.storage, CONFIG_KEY, &state)?;
    }

    let log_msg = if winning_amount.is_some() {
        bid_decimals = Some(state.bid_decimals);
        let seller_msg = if is_seller {
            ".  You have been sent the winning bid"
        } else {
            ""
        };
        let bidder_msg = if is_winner {
            ".  Your bid won! You have been sent the sale token(s)"
        } else if is_loser {
            ".  Your bid did not win and has been returned"
        } else {
            ""
        };
        format!("Sale has been finalized{}{}", seller_msg, bidder_msg)
    } else if return_all {
        "Outstanding funds have been returned".to_string()
    } else {
        let consign_msg = if no_bids && sell_tokens_received.is_some() {
            ".  Consigned tokens have been returned because there were no active bids"
        } else {
            ""
        };
        format!("Auction has been closed{}", consign_msg)
    };

    Ok(HandleResponse {
        messages: cos_msg,
        log: vec![],
        data: Some(to_binary(&HandleAnswer::CloseAuction {
            status: Success,
            message: log_msg,
            winning_bid: winning_amount,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        })?),
    })
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
        QueryMsg::AuctionInfo {} => try_query_info(deps),
        QueryMsg::ViewBid {
            address,
            viewing_key,
        } => try_view_bid(deps, &address, viewing_key),
        QueryMsg::HasBids {
            address,
            viewing_key,
        } => try_has_bids(deps, &address, viewing_key),
    };
    pad_query_result(response, BLOCK_SIZE)
}

/// Returns QueryResult displaying the auction information
///
/// # Arguments
///
/// * `deps` - reference to Extern containing all the contract's external dependencies
fn try_query_info<S: Storage, A: Api, Q: Querier>(deps: &Extern<S, A, Q>) -> QueryResult {
    let state: State = load(&deps.storage, CONFIG_KEY)?;

    // get sell token info
    let sell_token_info = state.sell_contract.token_info_query(&deps.querier)?;
    // get bid token info
    let bid_token_info = state.bid_contract.token_info_query(&deps.querier)?;

    // build status string
    let status = if state.is_completed {
        let locked = if !state.bidders.is_empty() || state.currently_consigned > 0 {
            ", but found outstanding balances.  Please run either retract_bid to \
                retrieve your non-winning bid, or return_all to return all outstanding bids/\
                consignment."
        } else {
            ""
        };
        format!("Closed{}", locked)
    } else {
        let consign = if !state.tokens_consigned { " NOT" } else { "" };
        format!(
            "Accepting bids: Token(s) to be sold have{} been consigned to the auction",
            consign
        )
    };

    let winning_bid = if state.winning_bid == 0 {
        None
    } else {
        Some(Uint128(state.winning_bid))
    };

    let ends_at = format!(
        "{} UTC",
        NaiveDateTime::from_timestamp(state.ends_at as i64, 0).format("%Y-%m-%d %H:%M:%S")
    );

    to_binary(&QueryAnswer::AuctionInfo {
        sell_token: Token {
            contract_address: state.sell_contract.address,
            token_info: sell_token_info,
        },
        bid_token: Token {
            contract_address: state.bid_contract.address,
            token_info: bid_token_info,
        },
        sell_amount: Uint128(state.sell_amount),
        minimum_bid: Uint128(state.minimum_bid),
        description: state.description,
        auction_address: state.auction_addr,
        ends_at,
        status,
        winning_bid,
    })
}

/// Returns QueryResult displaying the bid information
///
/// # Arguments
///
/// * `deps` - reference to Extern containing all the contract's external dependencies
/// * `bidder` - reference to address wanting to view its bid
/// * `key` - String holding the viewing key
fn try_view_bid<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    bidder: &HumanAddr,
    key: String,
) -> QueryResult {
    let state: State = load(&deps.storage, CONFIG_KEY)?;
    let key_valid_msg = FactoryQueryMsg::IsKeyValid {
        address: bidder.clone(),
        viewing_key: key,
    };
    let key_valid_response: IsKeyValidWrapper = key_valid_msg.query(
        &deps.querier,
        state.factory.code_hash,
        state.factory.address,
    )?;

    // if authenticated
    if key_valid_response.is_key_valid.is_valid {
        let decimals = state.bid_decimals;
        let bidder_raw = &deps.api.canonical_address(bidder)?;
        let mut amount_bid: Option<Uint128> = None;
        let mut message = String::new();
        let status: ResponseStatus;

        if state.bidders.contains(&bidder_raw.as_slice().to_vec()) {
            let bid: Option<Bid> = may_load(&deps.storage, bidder_raw.as_slice())?;
            if let Some(found_bid) = bid {
                status = Success;
                amount_bid = Some(Uint128(found_bid.amount));
                message.push_str(&format!(
                    "Bid placed {} UTC",
                    NaiveDateTime::from_timestamp(found_bid.timestamp as i64, 0)
                        .format("%Y-%m-%d %H:%M:%S")
                ));
            } else {
                status = Failure;
                message.push_str(&format!("No active bid for address: {}", bidder));
            }
        // no active bid found
        } else {
            status = Failure;
            message.push_str(&format!("No active bid for address: {}", bidder));
        }
        return to_binary(&QueryAnswer::Bid {
            status,
            message,
            amount_bid,
            bid_decimals: amount_bid.map(|_a| decimals),
        });
    }

    to_binary(&QueryAnswer::ViewingKeyError {
        error: "Wrong viewing key for this address or viewing key not set".to_string(),
    })
}

/// Returns QueryResult displaying the presence of active bids
///
/// # Arguments
///
/// * `deps` - reference to Extern containing all the contract's external dependencies
/// * `address` - a reference to the address claiming to be the seller
/// * `viewing_key` - String holding the viewing key
fn try_has_bids<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    address: &HumanAddr,
    viewing_key: String,
) -> QueryResult {
    let state: State = load(&deps.storage, CONFIG_KEY)?;
    let key_valid_msg = FactoryQueryMsg::IsKeyValid {
        address: address.clone(),
        viewing_key,
    };
    let key_valid_response: IsKeyValidWrapper = key_valid_msg.query(
        &deps.querier,
        state.factory.code_hash,
        state.factory.address,
    )?;

    // if authenticated
    if state.seller == *address && key_valid_response.is_key_valid.is_valid {
        return to_binary(&QueryAnswer::HasBids {
            has_bids: !state.bidders.is_empty(),
        });
    }

    to_binary(&QueryAnswer::ViewingKeyError {
        error: "Address and/or viewing key does not match auction creator's information"
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::ContractInfo;
    use cosmwasm_std::{
        from_binary, testing::*, BlockInfo, MessageInfo, QuerierResult, QueryResponse, StdResult,
    };
    use std::any::Any;

    fn init_helper() -> (
        StdResult<InitResponse>,
        Extern<MockStorage, MockApi, MockQuerier>,
    ) {
        let mut deps = mock_dependencies(20, &[]);
        let env = mock_env("factory", &[]);

        let factory = ContractInfo {
            code_hash: "factoryhash".to_string(),
            address: HumanAddr("factoryaddr".to_string()),
        };
        let sell_contract = ContractInfo {
            code_hash: "sellhash".to_string(),
            address: HumanAddr("selladdr".to_string()),
        };
        let bid_contract = ContractInfo {
            code_hash: "bidhash".to_string(),
            address: HumanAddr("bidaddr".to_string()),
        };
        let init_msg = InitMsg {
            factory,
            index: 0,
            label: "auction".to_string(),
            sell_symbol: 0,
            sell_decimals: 4,
            bid_symbol: 1,
            bid_decimals: 8,
            seller: HumanAddr("alice".to_string()),
            sell_contract,
            bid_contract,
            sell_amount: Uint128(10),
            minimum_bid: Uint128(10),
            ends_at: 1000,
            description: None,
        };
        (init(&mut deps, env, init_msg), deps)
    }

    fn extract_error_msg<T: Any>(error: StdResult<T>) -> String {
        match error {
            Ok(response) => {
                let bin_err = (&response as &dyn Any)
                    .downcast_ref::<QueryResponse>()
                    .expect("An error was expected, but no error could be extracted");
                match from_binary(bin_err).unwrap() {
                    QueryAnswer::ViewingKeyError { error } => error,
                    _ => panic!("Unexpected query answer"),
                }
            }
            Err(err) => match err {
                StdError::GenericErr { msg, .. } => msg,
                _ => panic!("Unexpected result from init"),
            },
        }
    }

    fn extract_log(resp: StdResult<HandleResponse>) -> String {
        match resp {
            Ok(response) => response.log[0].value.clone(),
            Err(_err) => "These are not the logs you are looking for".to_string(),
        }
    }

    fn extract_msg(resp: &StdResult<HandleResponse>) -> String {
        let handle_answer: HandleAnswer =
            from_binary(&resp.as_ref().unwrap().data.as_ref().unwrap()).unwrap();
        match handle_answer {
            HandleAnswer::Bid { message, .. } => message.clone(),
            HandleAnswer::Consign { message, .. } => message.clone(),
            HandleAnswer::CloseAuction { message, .. } => message.clone(),
            HandleAnswer::RetractBid { message, .. } => message.clone(),
            _ => panic!("Unexpected HandleAnswer"),
        }
    }

    fn extract_finalize_fields(
        resp: &StdResult<HandleResponse>,
    ) -> (
        String,
        Option<Uint128>,
        Option<u8>,
        Option<Uint128>,
        Option<u8>,
        Option<Uint128>,
    ) {
        let handle_answer: HandleAnswer =
            from_binary(&resp.as_ref().unwrap().data.as_ref().unwrap()).unwrap();
        match handle_answer {
            HandleAnswer::CloseAuction {
                message,
                winning_bid,
                bid_decimals,
                sell_tokens_received,
                sell_decimals,
                bid_tokens_received,
                ..
            } => (
                message,
                winning_bid,
                bid_decimals,
                sell_tokens_received,
                sell_decimals,
                bid_tokens_received,
            ),
            _ => panic!("Unexpected HandleAnswer"),
        }
    }

    fn extract_amount_returned(resp: &StdResult<HandleResponse>) -> (Option<Uint128>, Option<u8>) {
        let handle_answer: HandleAnswer =
            from_binary(&resp.as_ref().unwrap().data.as_ref().unwrap()).unwrap();
        match handle_answer {
            HandleAnswer::RetractBid {
                amount_returned,
                bid_decimals,
                ..
            } => (amount_returned, bid_decimals),
            _ => panic!("Unexpected HandleAnswer"),
        }
    }

    #[test]
    fn test_init_sanity() {
        let (init_result, deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        let factory = ContractInfo {
            code_hash: "factoryhash".to_string(),
            address: HumanAddr("factoryaddr".to_string()),
        };
        let sell_contract = ContractInfo {
            code_hash: "sellhash".to_string(),
            address: HumanAddr("selladdr".to_string()),
        };
        let bid_contract = ContractInfo {
            code_hash: "bidhash".to_string(),
            address: HumanAddr("bidaddr".to_string()),
        };

        assert_eq!(factory, state.factory);
        assert_eq!(HumanAddr("alice".to_string()), state.seller);
        assert_eq!(sell_contract, state.sell_contract);
        assert_eq!(4, state.sell_decimals);
        assert_eq!(bid_contract, state.bid_contract);
        assert_eq!(8, state.bid_decimals);
        assert_eq!(10, state.sell_amount);
        assert_eq!(10, state.minimum_bid);
        assert_eq!(0, state.currently_consigned);
        assert_eq!(HashSet::new(), state.bidders);
        assert_eq!(false, state.is_completed);
        assert_eq!(false, state.tokens_consigned);
        assert_eq!(1000, state.ends_at);
        assert_eq!(None, state.description);
        assert_eq!(0, state.winning_bid);
    }

    #[test]
    fn test_consign() {
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        // try to consign if not seller
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(2500),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let error = extract_error_msg(handle_result);
        assert!(error.contains("Only auction creator can consign tokens for sale"));

        // try already consigned
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(2500),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(2500),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let error = extract_error_msg(handle_result);
        assert!(error.contains("Tokens to be sold have already been consigned."));

        // try to consign after closing
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let _used = handle(&mut deps, mock_env("alice", &[]), handle_msg);
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(2500),
            msg: None,
        };

        let handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let error = extract_error_msg(handle_result);
        assert!(error.contains("Auction has ended. Your tokens have been returned"));

        // try consign too little
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(2),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("not consigned the full amount"));
        assert!(log.contains("\"amount_needed\":\"8\""));
        assert!(log.contains("\"sell_decimals\":4"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(false, state.tokens_consigned);

        // try consign too much
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("Excess tokens have been returned"));
        assert!(log.contains("\"amount_returned\":\"12\""));
        assert!(log.contains("\"sell_decimals\":4"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert!(state.tokens_consigned);
    }

    #[test]
    fn test_bid() {
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        // try bid 0
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(0),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let error = extract_error_msg(handle_result);
        assert!(error.contains("Bid must be greater than 0"));

        // try bid less than minimum
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(9),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("Bid was less than minimum allowed"));
        assert!(log.contains("\"minimum_bid\":\"10\""));
        assert!(log.contains("\"amount_returned\":\"9\""));
        assert!(log.contains("\"bid_decimals\":8"));

        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 0);

        // sanity check
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("\"amount_bid\":\"100\""));
        assert!(log.contains("\"bid_decimals\":8"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 1);
        let bid: Bid = load(
            &deps.storage,
            deps.api
                .canonical_address(&HumanAddr("bob".to_string()))
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(bid.amount, 100);

        // test bid equal to  previous bid
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("New bid is the same as previous bid.  Retaining previous timestamp"));
        assert!(log.contains("\"previous_bid\":\"100\""));
        assert!(log.contains("\"amount_bid\":\"100\""));
        assert!(log.contains("\"amount_returned\":\"100\""));
        assert!(log.contains("\"bid_decimals\":8"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 1);

        // test bid less than previous bid
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(25),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("Previously bid tokens have been returned"));
        assert!(log.contains("\"amount_bid\":\"25\""));
        assert!(log.contains("\"amount_returned\":\"100\""));
        assert!(log.contains("\"bid_decimals\":8"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 1);

        // test bid more than previous bid
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(250),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("Previously bid tokens have been returned"));
        assert!(log.contains("\"amount_bid\":\"250\""));
        assert!(log.contains("\"amount_returned\":\"25\""));
        assert!(log.contains("\"bid_decimals\":8"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 1);

        // try bid after close
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: Some(2000),
            new_minimum_bid: Some(Uint128(1000)),
        };
        let _used = handle(&mut deps, mock_env("alice", &[]), handle_msg);
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(2500),
            msg: None,
        };

        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let error = extract_error_msg(handle_result);
        assert!(error.contains("Auction has ended"));
    }

    #[test]
    fn test_change_min_bid() {
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        // try change min bid not seller
        let handle_msg = HandleMsg::ChangeMinimumBid {
            minimum_bid: Uint128(20),
        };
        let handle_result = handle(&mut deps, mock_env("bob", &[]), handle_msg);
        let error = extract_error_msg(handle_result);
        assert!(error.contains("Only the auction seller can change the minimum bid"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.minimum_bid, 10);

        // try change min bid after close
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1000,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, None);
        assert_eq!(bid_decimals, None);
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains("Auction has been closed"));
        assert!(!message.contains("Auction has been closed."));

        let handle_msg = HandleMsg::ChangeMinimumBid {
            minimum_bid: Uint128(20),
        };
        let handle_result = handle(&mut deps, mock_env("alice", &[]), handle_msg);
        let error = extract_error_msg(handle_result);
        assert!(error.contains("Can not change the minimum bid of an auction that has ended"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.minimum_bid, 10);

        // sanity check
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(10),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("\"amount_bid\":\"10\""));
        assert!(log.contains("\"bid_decimals\":8"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 1);

        let handle_msg = HandleMsg::ChangeMinimumBid {
            minimum_bid: Uint128(20),
        };
        let _handle_result = handle(&mut deps, mock_env("alice", &[]), handle_msg);
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.minimum_bid, 20);

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("charlie".to_string()),
            amount: Uint128(15),
            msg: None,
        };
        let handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let log = extract_log(handle_result);
        assert!(log.contains("Bid was less than minimum allowed"));
        assert!(log.contains("\"minimum_bid\":\"20\""));
        assert!(log.contains("\"amount_returned\":\"15\""));
        assert!(log.contains("\"bid_decimals\":8"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 1);
    }

    #[test]
    fn test_retract_bid() {
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        // try no bid placed
        let handle_msg = HandleMsg::RetractBid {};
        let handle_result = handle(&mut deps, mock_env("bob", &[]), handle_msg);
        let message = extract_msg(&handle_result);
        assert!(message.contains("No active bid for address"));
        let (amount, decimals) = extract_amount_returned(&handle_result);
        assert_eq!(amount, None);
        assert_eq!(decimals, None);

        // sanity check
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("bidaddr", &[]), handle_msg);
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 1);
        let bid: Bid = load(
            &deps.storage,
            deps.api
                .canonical_address(&HumanAddr("bob".to_string()))
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(bid.amount, 100);

        let handle_msg = HandleMsg::RetractBid {};
        let handle_result = handle(&mut deps, mock_env("bob", &[]), handle_msg);
        let message = extract_msg(&handle_result);
        assert!(message.contains("Bid retracted.  Tokens have been returned"));
        let (amount, decimals) = extract_amount_returned(&handle_result);
        assert_eq!(amount, Some(Uint128(100)));
        assert_eq!(decimals, Some(8));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 0);
    }

    #[test]
    fn test_finalize() {
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        // try return all before closing
        let handle_msg = HandleMsg::ReturnAll {};
        let handle_result = handle(&mut deps, mock_env("bob", &[]), handle_msg);
        let error = extract_error_msg(handle_result);
        assert!(error.contains("return_all can only be executed after the auction has ended"));

        // try non-seller closing before end time
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let error = extract_error_msg(handle_result);
        assert!(
            error.contains("Only auction creator can finalize the sale before the closing time")
        );

        // try non-seller closing after end time
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1000,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, None);
        assert_eq!(bid_decimals, None);
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains("Auction has been closed"));
        assert!(!message.contains("Auction has been closed."));

        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        // try stranger not wanting to close without bids
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: Some(2000),
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1100,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let error = extract_error_msg(handle_result);
        assert!(error
            .contains("Only the auction seller can change the closing time or the minimum bid"));
        // try stranger not wanting to close without bids
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: Some(Uint128(1000)),
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1100,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let error = extract_error_msg(handle_result);
        assert!(error
            .contains("Only the auction seller can change the closing time or the minimum bid"));
        // try stranger not wanting to close without bids
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: Some(2000),
            new_minimum_bid: Some(Uint128(1000)),
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1100,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let error = extract_error_msg(handle_result);
        assert!(error
            .contains("Only the auction seller can change the closing time or the minimum bid"));

        // try seller not wanting to close without bids
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: Some(2000),
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("alice".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, None);
        assert_eq!(bid_decimals, None);
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains("There were no active bids.  The closing time has been updated"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.ends_at, 2000);
        assert_eq!(state.minimum_bid, 10);

        // try seller not wanting to close without bids
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: Some(Uint128(1000)),
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("alice".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, None);
        assert_eq!(bid_decimals, None);
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains("There were no active bids.  The minimum bid has been updated"));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.ends_at, 2000);
        assert_eq!(state.minimum_bid, 1000);

        // try seller not wanting to close without bids
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: Some(10000),
            new_minimum_bid: Some(Uint128(100000)),
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("alice".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, None);
        assert_eq!(bid_decimals, None);
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains(
            "There were no active bids.  The closing time and minimum bid has been updated"
        ));
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.ends_at, 10000);
        assert_eq!(state.minimum_bid, 100000);

        // test 3 bidders, highest retracts, other two are tied with time tie-breaker
        // winner closes after end time
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 10,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("charlie".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 12,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("david".to_string()),
            amount: Uint128(1000),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 5,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 3);
        let handle_msg = HandleMsg::RetractBid {};
        let _handle_result = handle(&mut deps, mock_env("david", &[]), handle_msg);
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 2);

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 2000,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, Some(Uint128(100)));
        assert_eq!(bid_decimals, Some(8));
        assert_eq!(sell_tokens_received, Some(Uint128(10)));
        assert_eq!(sell_decimals, Some(4));
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains(
            "Sale has been finalized.  Your bid won! You have been sent the sale token(s)"
        ));

        // test 3 bidders, highest is seller, seller closes
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 10,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("charlie".to_string()),
            amount: Uint128(200),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 12,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(1000),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 5,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 3);

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: Some(2000),
            new_minimum_bid: Some(Uint128(1000)),
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("alice".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, Some(Uint128(1000)));
        assert_eq!(bid_decimals, Some(8));
        assert_eq!(sell_tokens_received, Some(Uint128(10)));
        assert_eq!(sell_decimals, Some(4));
        assert_eq!(bid_tokens_received, Some(Uint128(1000)));
        assert!(message.contains("Sale has been finalized.  You have been sent the winning bid.  Your bid won! You have been sent the sale token(s)"));

        // test 3 bidders, seller loses, seller closes
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(1000),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 10,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("charlie".to_string()),
            amount: Uint128(200),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 12,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(25),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 5,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 3);

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: Some(2000),
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("alice".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, Some(Uint128(1000)));
        assert_eq!(bid_decimals, Some(8));
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, Some(Uint128(1025)));
        assert!(message.contains("Sale has been finalized.  You have been sent the winning bid.  Your bid did not win and has been returned"));

        // test 3 bidders, loser closes
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(500),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 10,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("charlie".to_string()),
            amount: Uint128(500),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 12,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("david".to_string()),
            amount: Uint128(25),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 5,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 3);

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1000,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("charlie".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, Some(Uint128(500)));
        assert_eq!(bid_decimals, Some(8));
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, Some(Uint128(500)));
        assert!(message
            .contains("Sale has been finalized.  Your bid did not win and has been returned"));

        // test 3 bidders, seller closes
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(1000),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 10,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("charlie".to_string()),
            amount: Uint128(200),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 12,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("david".to_string()),
            amount: Uint128(25),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 5,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 3);

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1000,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("alice".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, Some(Uint128(1000)));
        assert_eq!(bid_decimals, Some(8));
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, Some(Uint128(1000)));
        assert!(message.contains("Sale has been finalized.  You have been sent the winning bid"));
        assert!(!message.contains("Sale has been finalized.  You have been sent the winning bid."));

        // return all response
        let handle_msg = HandleMsg::ReturnAll {};
        let handle_result = handle(&mut deps, mock_env("bob", &[]), handle_msg);
        let message = extract_msg(&handle_result);
        assert!(message.contains("Outstanding funds have been returned"));

        // test 3 bidders, stranger closes
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(1000),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 10,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("charlie".to_string()),
            amount: Uint128(200),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 12,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("david".to_string()),
            amount: Uint128(25),
            msg: None,
        };
        let _handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 5,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bidaddr".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let state: State = load(&deps.storage, CONFIG_KEY).unwrap();
        assert_eq!(state.bidders.len(), 3);

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1000,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("edmund".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, Some(Uint128(1000)));
        assert_eq!(bid_decimals, Some(8));
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains("Sale has been finalized"));
        assert!(!message.contains("Sale has been finalized."));

        // test already closed, stranger closes
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1000,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, None);
        assert_eq!(bid_decimals, None);
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains("Auction has been closed"));
        assert!(!message.contains("Auction has been closed."));

        // test no bidders, seller closes
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 10,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("alice".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, None);
        assert_eq!(bid_decimals, None);
        assert_eq!(sell_tokens_received, Some(Uint128(10)));
        assert_eq!(sell_decimals, Some(4));
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains("Auction has been closed.  Consigned tokens have been returned because there were no active bids"));

        // test no bidders, stranger closes
        let (init_result, mut deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("alice".to_string()),
            amount: Uint128(20),
            msg: None,
        };
        let _handle_result = handle(&mut deps, mock_env("selladdr", &[]), handle_msg);
        let handle_msg = HandleMsg::Finalize {
            new_ends_at: None,
            new_minimum_bid: None,
        };
        let handle_result = handle(
            &mut deps,
            Env {
                block: BlockInfo {
                    height: 12_345,
                    time: 1000,
                    chain_id: "cosmos-testnet-14002".to_string(),
                },
                message: MessageInfo {
                    sender: HumanAddr("bob".to_string()),
                    sent_funds: vec![],
                },
                contract: cosmwasm_std::ContractInfo {
                    address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                },
                contract_key: Some("".to_string()),
                contract_code_hash: "".to_string(),
            },
            handle_msg,
        );
        let (
            message,
            winning_bid,
            bid_decimals,
            sell_tokens_received,
            sell_decimals,
            bid_tokens_received,
        ) = extract_finalize_fields(&handle_result);
        assert_eq!(winning_bid, None);
        assert_eq!(bid_decimals, None);
        assert_eq!(sell_tokens_received, None);
        assert_eq!(sell_decimals, None);
        assert_eq!(bid_tokens_received, None);
        assert!(message.contains("Auction has been closed"));
        assert!(!message.contains("Auction has been closed."));
    }

    #[test]
    fn test_query_view_bid() {
        let (init_result, deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        #[derive(Debug)]
        struct MyMockQuerier {
            pub is_valid: bool,
        }
        impl Querier for MyMockQuerier {
            fn raw_query(&self, _request: &[u8]) -> QuerierResult {
                Ok(to_binary(&IsKeyValidWrapper {
                    is_key_valid: IsKeyValid {
                        is_valid: self.is_valid,
                    },
                }))
            }
        }
        let invalid_deps = deps.change_querier(|_| MyMockQuerier { is_valid: false });
        // try wrong key
        let query_msg = QueryMsg::ViewBid {
            address: HumanAddr("bob".to_string()),
            viewing_key: "wrong_key".to_string(),
        };
        let query_result = query(&invalid_deps, query_msg);
        let error = extract_error_msg(query_result);
        assert!(error.contains("Wrong viewing key"));

        let (init_result, deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        let valid_deps = deps.change_querier(|_| MyMockQuerier { is_valid: true });
        // try no bid
        let query_msg = QueryMsg::ViewBid {
            address: HumanAddr("bob".to_string()),
            viewing_key: "key".to_string(),
        };
        let query_result = query(&valid_deps, query_msg);
        let (message, bid_decimals) = match from_binary(&query_result.unwrap()).unwrap() {
            QueryAnswer::Bid {
                message,
                bid_decimals,
                ..
            } => (message, bid_decimals),
            _ => panic!("Unexpected"),
        };
        assert!(message.contains("No active bid for address: bob"));
        assert_eq!(bid_decimals, None);

        // sanity check
        let (init_result, deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        let mut valid_deps = deps.change_querier(|_| MyMockQuerier { is_valid: true });
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let _handle_result = handle(&mut valid_deps, mock_env("bidaddr", &[]), handle_msg);
        let query_msg = QueryMsg::ViewBid {
            address: HumanAddr("bob".to_string()),
            viewing_key: "key".to_string(),
        };
        let query_result = query(&valid_deps, query_msg);
        let (message, amount_bid, bid_decimals) = match from_binary(&query_result.unwrap()).unwrap()
        {
            QueryAnswer::Bid {
                message,
                amount_bid,
                bid_decimals,
                ..
            } => (message, amount_bid, bid_decimals),
            _ => panic!("Unexpected"),
        };
        assert!(message.contains("Bid placed"));
        assert_eq!(amount_bid, Some(Uint128(100)));
        assert_eq!(bid_decimals, Some(8));
    }

    #[test]
    fn test_query_has_bids() {
        let (init_result, deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );

        #[derive(Debug)]
        struct MyMockQuerier {
            pub is_valid: bool,
        }
        impl Querier for MyMockQuerier {
            fn raw_query(&self, _request: &[u8]) -> QuerierResult {
                Ok(to_binary(&IsKeyValidWrapper {
                    is_key_valid: IsKeyValid {
                        is_valid: self.is_valid,
                    },
                }))
            }
        }
        let invalid_deps = deps.change_querier(|_| MyMockQuerier { is_valid: false });
        // try wrong key
        let query_msg = QueryMsg::HasBids {
            address: HumanAddr("alice".to_string()),
            viewing_key: "wrong_key".to_string(),
        };
        let query_result = query(&invalid_deps, query_msg);
        let error = extract_error_msg(query_result);
        assert!(error
            .contains("Address and/or viewing key does not match auction creator's information"));

        let (init_result, deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        let valid_deps = deps.change_querier(|_| MyMockQuerier { is_valid: true });
        // try not seller
        let query_msg = QueryMsg::HasBids {
            address: HumanAddr("bob".to_string()),
            viewing_key: "key".to_string(),
        };
        let query_result = query(&valid_deps, query_msg);
        let error = extract_error_msg(query_result);
        assert!(error
            .contains("Address and/or viewing key does not match auction creator's information"));

        // sanity check, no bids
        let (init_result, deps) = init_helper();
        assert!(
            init_result.is_ok(),
            "Init failed: {}",
            init_result.err().unwrap()
        );
        let mut valid_deps = deps.change_querier(|_| MyMockQuerier { is_valid: true });
        let query_msg = QueryMsg::HasBids {
            address: HumanAddr("alice".to_string()),
            viewing_key: "key".to_string(),
        };
        let query_result = query(&valid_deps, query_msg);
        let has_bids = match from_binary(&query_result.unwrap()).unwrap() {
            QueryAnswer::HasBids { has_bids, .. } => has_bids,
            _ => panic!("Unexpected"),
        };
        assert!(!has_bids);

        // sanity check, with bids
        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("bob".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let _handle_result = handle(&mut valid_deps, mock_env("bidaddr", &[]), handle_msg);
        let query_msg = QueryMsg::HasBids {
            address: HumanAddr("alice".to_string()),
            viewing_key: "key".to_string(),
        };
        let query_result = query(&valid_deps, query_msg);
        let has_bids = match from_binary(&query_result.unwrap()).unwrap() {
            QueryAnswer::HasBids { has_bids, .. } => has_bids,
            _ => panic!("Unexpected"),
        };
        assert!(has_bids);

        let handle_msg = HandleMsg::Receive {
            sender: HumanAddr("blah".to_string()),
            from: HumanAddr("charlie".to_string()),
            amount: Uint128(100),
            msg: None,
        };
        let _handle_result = handle(&mut valid_deps, mock_env("bidaddr", &[]), handle_msg);
        let query_msg = QueryMsg::HasBids {
            address: HumanAddr("alice".to_string()),
            viewing_key: "key".to_string(),
        };
        let query_result = query(&valid_deps, query_msg);
        let has_bids = match from_binary(&query_result.unwrap()).unwrap() {
            QueryAnswer::HasBids { has_bids, .. } => has_bids,
            _ => panic!("Unexpected"),
        };
        assert!(has_bids);
    }
}
