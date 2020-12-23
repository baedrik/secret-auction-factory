use serde::Serialize;

use cosmwasm_std::{
    log, to_binary, Api, CanonicalAddr, Env, Extern, HandleResponse, HandleResult, HumanAddr,
    InitResponse, InitResult, Querier, QueryResult, StdError, Storage, Uint128,
};
use cosmwasm_storage::{PrefixedStorage, ReadonlyPrefixedStorage};

use std::collections::HashSet;

use serde_json_wasm as serde_json;

use secret_toolkit::utils::{pad_handle_result, pad_query_result, HandleCallback};

use crate::msg::{
    HandleAnswer, HandleMsg, InitMsg, QueryAnswer, QueryMsg, ResponseStatus,
    ResponseStatus::{Failure, Success},
    Token,
};
use crate::state::{load, may_load, remove, save, Bid, State};
use crate::viewing_key::{ViewingKey, VIEWING_KEY_SIZE};

use chrono::NaiveDateTime;

/// storage key for auction state
pub const CONFIG_KEY: &[u8] = b"config";
/// prefix for viewing keys
pub const PREFIX_VIEW_KEY: &[u8] = b"viewingkey";

/// pad handle responses and log attributes to blocks of 256 bytes to prevent leaking info based on
/// response size
pub const BLOCK_SIZE: usize = 256;

/// auction info
#[derive(Serialize)]
pub struct FactoryAuctionInfo {
    /// auction label
    pub label: String,
    /// sell symbol index
    pub sell_symbol: u16,
    /// bid symbol index
    pub bid_symbol: u16,
    /// auction contract version number
    pub version: u8,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryHandleMsg {
    /// RegisterAuction saves the auction info of a newly instantiated auction
    RegisterAuction {
        seller: HumanAddr,
        auction: FactoryAuctionInfo,
    },
    CloseAuction {
        /// auction seller
        seller: HumanAddr,
        /// winning bidder if the auction ended in a swap
        bidder: Option<HumanAddr>,
        /// winning bid if the auction ended in a swap
        winning_bid: Option<Uint128>,
    },

    RegisterBidder {
        bidder: HumanAddr,
    },
    RemoveBidder {
        bidder: HumanAddr,
    },
}

impl HandleCallback for FactoryHandleMsg {
    const BLOCK_SIZE: usize = BLOCK_SIZE;
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
        auction_addr: env.contract.address,
        seller: msg.seller.clone(),
        sell_contract: msg.sell_contract,
        bid_contract: msg.bid_contract,
        sell_amount: msg.sell_amount.u128(),
        minimum_bid: msg.minimum_bid.u128(),
        currently_consigned: 0,
        bidders: HashSet::new(),
        is_completed: false,
        tokens_consigned: false,
        description: msg.description,
        winning_bid: 0,
    };

    save(&mut deps.storage, CONFIG_KEY, &state)?;

    let auction = FactoryAuctionInfo {
        label: msg.label,
        sell_symbol: msg.sell_symbol,
        bid_symbol: msg.bid_symbol,
        version: msg.version,
    };

    let reg_auction_msg = FactoryHandleMsg::RegisterAuction {
        seller: msg.seller,
        auction,
    };
    // perform factory register callback
    let cosmos_msg =
        reg_auction_msg.to_cosmos_msg(msg.factory.code_hash, msg.factory.address, None)?;
    // register receive with the bid/sell token contracts
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
        HandleMsg::Finalize { only_if_bids, .. } => try_finalize(deps, env, only_if_bids, false),
        HandleMsg::ReturnAll { .. } => try_finalize(deps, env, false, true),
        HandleMsg::Receive { from, amount, .. } => try_receive(deps, env, from, amount),
        HandleMsg::SetViewingKey { key, bidder } => try_set_key(deps, env, &bidder, key),
    };
    pad_handle_result(response, BLOCK_SIZE)
}

fn try_set_key<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    bidder: &HumanAddr,
    key: String,
) -> HandleResult {
    let state: State = load(&deps.storage, CONFIG_KEY)?;
    // only allow the factory to set the viewing key
    if env.message.sender != state.factory.address {
        return Err(StdError::generic_err(
            "Only the factory can set the viewing key in order to ensure all auctions use the same key.",
        ));
    }

    let vk = ViewingKey(key);
    let bidder_raw = deps.api.canonical_address(bidder)?;
    let mut key_store = PrefixedStorage::new(PREFIX_VIEW_KEY, &mut deps.storage);
    save(&mut key_store, bidder_raw.as_slice(), &vk.to_hashed())?;

    Ok(HandleResponse {
        messages: vec![],

        //////////////
        log: vec![log("vkey", format!("{}", vk))],
        data: None,
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
        let resp = serde_json::to_string(&HandleAnswer::Status {
            status: Failure,
            message,
        })
        .unwrap();

        Ok(HandleResponse {
            messages: vec![],
            log: vec![log("response", resp)],
            data: None,
        })
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
fn try_consign<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    owner: HumanAddr,
    amount: Uint128,
    state: &mut State,
) -> HandleResult {
    // if not the auction owner, send the tokens back
    if owner != state.seller {
        let message = String::from(
            "Only auction creator can consign tokens for sale.  Your tokens have been returned",
        );

        let resp = serde_json::to_string(&HandleAnswer::Consign {
            status: Failure,
            message,
            amount_consigned: None,
            amount_needed: None,
            amount_returned: Some(amount),
        })
        .unwrap();

        return Ok(HandleResponse {
            messages: vec![state.sell_contract.transfer_msg(owner, amount)?],
            log: vec![log("response", resp)],
            data: None,
        });
    }
    // if auction is over, send the tokens back
    if state.is_completed {
        let message = String::from("Auction has ended. Your tokens have been returned");

        let resp = serde_json::to_string(&HandleAnswer::Consign {
            status: Failure,
            message,
            amount_consigned: None,
            amount_needed: None,
            amount_returned: Some(amount),
        })
        .unwrap();

        return Ok(HandleResponse {
            messages: vec![state.sell_contract.transfer_msg(owner, amount)?],
            log: vec![log("response", resp)],
            data: None,
        });
    }
    // if tokens to be sold have already been consigned, return these tokens
    if state.tokens_consigned {
        let message = String::from(
            "Tokens to be sold have already been consigned. Your tokens have been returned",
        );

        let resp = serde_json::to_string(&HandleAnswer::Consign {
            status: Failure,
            message,
            amount_consigned: Some(Uint128(state.currently_consigned)),
            amount_needed: None,
            amount_returned: Some(amount),
        })
        .unwrap();

        return Ok(HandleResponse {
            messages: vec![state.sell_contract.transfer_msg(owner, amount)?],
            log: vec![log("response", resp)],
            data: None,
        });
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
        amount_consigned: Some(Uint128(state.currently_consigned)),
        amount_needed: needed,
        amount_returned: excess,
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
        let message = String::from("Auction has ended. Bid tokens have been returned");

        let resp = serde_json::to_string(&HandleAnswer::Bid {
            status: Failure,
            message,
            previous_bid: None,
            minimum_bid: None,
            amount_bid: None,
            amount_returned: Some(amount),
        })
        .unwrap();

        return Ok(HandleResponse {
            messages: vec![state.bid_contract.transfer_msg(bidder, amount)?],
            log: vec![log("response", resp)],
            data: None,
        });
    }
    // don't accept a 0 bid
    if amount == Uint128(0) {
        let message = String::from("Bid must be greater than 0");

        let resp = serde_json::to_string(&HandleAnswer::Bid {
            status: Failure,
            message,
            previous_bid: None,
            minimum_bid: None,
            amount_bid: None,
            amount_returned: None,
        })
        .unwrap();

        return Ok(HandleResponse {
            messages: vec![],
            log: vec![log("response", resp)],
            data: None,
        });
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
            // if new bid is <= the old bid, keep old bid and return this one
            if amount.u128() <= old_bid.amount {
                let message = String::from(
                    "New bid less than or equal to previous bid. Newly bid tokens have been \
                     returned",
                );

                let resp = serde_json::to_string(&HandleAnswer::Bid {
                    status: Failure,
                    message,
                    previous_bid: Some(Uint128(old_bid.amount)),
                    minimum_bid: None,
                    amount_bid: None,
                    amount_returned: Some(amount),
                })
                .unwrap();

                return Ok(HandleResponse {
                    messages: vec![state.bid_contract.transfer_msg(bidder, amount)?],
                    log: vec![log("response", resp)],
                    data: None,
                });
            // new bid is larger, save the new bid, and return the old one, so mark for return
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
            let rem_bid_msg = FactoryHandleMsg::RemoveBidder { bidder };
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
/// * `only_if_bids` - true if auction should stay open if there are no bids
/// * `return_all` - true if being called from the return_all fallback plan
fn try_finalize<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    only_if_bids: bool,
    return_all: bool,
) -> HandleResult {

let mut tmplog = Vec::new();


    let mut state: State = load(&deps.storage, CONFIG_KEY)?;

    // can only do a return_all if the auction is closed
    if return_all && !state.is_completed {
        return Ok(HandleResponse {
            messages: vec![],
            log: vec![],
            data: Some(to_binary(&HandleAnswer::CloseAuction {
                status: Failure,
                message: String::from(
                    "return_all can only be executed after the auction has ended",
                ),
                winning_bid: None,
                amount_returned: None,
            })?),
        });
    }
    // if not the auction owner, can't finalize, but you can return_all
    if !return_all && env.message.sender != state.seller {
        return Ok(HandleResponse {
            messages: vec![],
            log: vec![],
            data: Some(to_binary(&HandleAnswer::CloseAuction {
                status: Failure,
                message: String::from("Only auction creator can finalize the sale"),
                winning_bid: None,
                amount_returned: None,
            })?),
        });
    }
    // if there are no active bids, and owner only wants to close if bids
    if !state.is_completed && only_if_bids && state.bidders.is_empty() {
        return Ok(HandleResponse {
            messages: vec![],
            log: vec![],
            data: Some(to_binary(&HandleAnswer::CloseAuction {
                status: Failure,
                message: String::from("Did not close because there are no active bids"),
                winning_bid: None,
                amount_returned: None,
            })?),
        });
    }
    let mut cos_msg = Vec::new();
    let mut update_state = false;
    let mut winning_amount: Option<Uint128> = None;
    let mut winner: Option<HumanAddr> = None;
    let mut amount_returned: Option<Uint128> = None;

    let no_bids = state.bidders.is_empty();
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
                state.currently_consigned = 0;
                update_state = true;
                winning_amount = Some(Uint128(winning_bid.bid.amount));
                winner = Some(human_winner);
                state.winning_bid = winning_bid.bid.amount;
                remove(&mut deps.storage, &winning_bid.bidder.as_slice());
                state
                    .bidders
                    .remove(&winning_bid.bidder.as_slice().to_vec());
tmplog.push(log("here", "winner processing"));

            }
        }
        // loops through all remaining bids to return them to the bidders
        for losing_bid in &bid_list {
            cos_msg.push(state.bid_contract.transfer_msg(
                deps.api.human_address(&losing_bid.bidder)?,
                Uint128(losing_bid.bid.amount),
            )?);
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
        if !return_all {
            amount_returned = Some(Uint128(state.currently_consigned));
        }
        state.currently_consigned = 0;
        update_state = true;
    }
    // mark that auction had ended
    if !state.is_completed {
tmplog.push(log("here", "it completed"));
        state.is_completed = true;
        update_state = true;
        let close_msg = FactoryHandleMsg::CloseAuction {
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
tmplog.push(log("cosmsgs",format!("{:?}",cos_msg)));
    }
    if update_state {
        save(&mut deps.storage, CONFIG_KEY, &state)?;
    }

    let log_msg = if winning_amount.is_some() {
        "Sale finalized.  You have been sent the winning bid tokens".to_string()
    } else if amount_returned.is_some() {
        let cause = if !state.tokens_consigned {
            " because you did not consign the full sale amount"
        } else if no_bids {
            " because there were no active bids"
        } else {
            ""
        };
        format!(
            "Auction closed.  You have been returned the consigned tokens{}",
            cause
        )
    } else if return_all {
        "Outstanding funds have been returned".to_string()
    } else {
        "Auction has been closed".to_string()
    };
    Ok(HandleResponse {
        messages: cos_msg,
        log: tmplog,
        data: Some(to_binary(&HandleAnswer::CloseAuction {
            status: Success,
            message: log_msg,
            winning_bid: winning_amount,
            amount_returned,
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
        QueryMsg::AuctionInfo { .. } => try_query_info(deps),
        QueryMsg::ViewBid { address, viewing_key } => try_view_bid(deps, &address, viewing_key),
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
    let bidder_raw = &deps.api.canonical_address(bidder)?;
    let read_key = ReadonlyPrefixedStorage::new(PREFIX_VIEW_KEY, &deps.storage);
    let load_key: Option<[u8; VIEWING_KEY_SIZE]> = may_load(&read_key, bidder_raw.as_slice())?;

    let input_key = ViewingKey(key);
    // if a key was set
    if let Some(expected_key) = load_key {
        // and it matches
        if input_key.check_viewing_key(&expected_key) {
            let state: State = load(&deps.storage, CONFIG_KEY)?;
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
            });
        }
    } else {
        // Checking the key will take significant time. We don't want to exit immediately if it isn't set
        // in a way which will allow to time the command and determine if a viewing key doesn't exist
        input_key.check_viewing_key(&[0u8; VIEWING_KEY_SIZE]);
    }

    to_binary(&QueryAnswer::ViewingKeyError {
        error: "Wrong viewing key for this address or viewing key not set".to_string(),
    })
}