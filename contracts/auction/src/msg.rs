use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use cosmwasm_std::{Binary, CosmosMsg, HumanAddr, Querier, StdResult, Uint128};

use secret_toolkit::snip20::{register_receive_msg, token_info_query, transfer_msg, TokenInfo};

use crate::contract::BLOCK_SIZE;

/// Instantiation message
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct InitMsg {
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

/// Handle messages
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandleMsg {
    /// Receive gets called by the token contracts of the auction.  If it came from the sale token, it
    /// will consign the sent tokens.  If it came from the bid token, it will place a bid.  If any
    /// other address tries to call this, it will give an error message that the calling address is
    /// not a token in the auction.
    Receive {
        /// address of person or contract that sent the tokens that triggered this Receive
        sender: HumanAddr,
        /// address of the owner of the tokens sent to the auction
        from: HumanAddr,
        /// amount of tokens sent
        amount: Uint128,
        /// Optional base64 encoded message sent with the Send call -- not needed or used by this
        /// contract
        #[serde(default)]
        msg: Option<Binary>,
    },

    /// RetractBid will retract any active bid the calling address has made and return the tokens
    /// that are held in escrow
    RetractBid {},

    /// Finalize will close the auction
    Finalize {
        /// optional timestamp to extend the closing time to if there are no bids. Timestamp is in
        /// seconds since epoch 01/01/1970
        #[serde(default)]
        new_ends_at: Option<u64>,
        /// optional minimum bid update if there are no bids
        #[serde(default)]
        new_minimum_bid: Option<Uint128>,
    },

    /// If the auction holds any funds after it has closed (should never happen), this will return
    /// those funds to their owners.  Should never be needed, but included in case of unforeseen
    /// error
    ReturnAll {},

    /// ChangeMinimumBid allows the seller to change the minimum bid.  The new minimum bid only
    /// applies to new bids placed.  Any bid that were already accepted, will still be considered
    /// valid bids
    ChangeMinimumBid {
        /// new minimum bid
        minimum_bid: Uint128,
    },
}

/// Queries
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    /// Displays the auction information
    AuctionInfo {},
    /// View active bid for input address
    ViewBid {
        /// address whose bid should be displayed
        address: HumanAddr,
        /// bidder's viewing key
        viewing_key: String,
    },
    /// returns boolean indicating whether there are any active bids
    HasBids {
        /// address to authenticate as the auction seller
        address: HumanAddr,
        /// seller's viewing key
        viewing_key: String,
    },
}

/// responses to queries
#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryAnswer {
    /// AuctionInfo query response
    AuctionInfo {
        /// sell token address and TokenInfo query response
        sell_token: Token,
        /// bid token address and TokenInfo query response
        bid_token: Token,
        /// amount of tokens being sold
        sell_amount: Uint128,
        /// minimum bid that will be accepted
        minimum_bid: Uint128,
        /// Optional String description of auction
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// address of auction contract
        auction_address: HumanAddr,
        /// time at which anyone can close the auction
        ends_at: String,
        /// status of the auction can be "Accepting bids: Tokens to be sold have(not) been
        /// consigned" or "Closed" (will also state if there are outstanding funds after auction
        /// closure
        status: String,
        /// If the auction resulted in a swap, this will state the winning bid
        #[serde(skip_serializing_if = "Option::is_none")]
        winning_bid: Option<Uint128>,
    },
    /// response from view bid attempt
    Bid {
        /// success or failure
        status: ResponseStatus,
        /// execution description
        message: String,
        /// Optional amount bid
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_bid: Option<Uint128>,
        /// Optional number of decimals in bid amount
        #[serde(skip_serializing_if = "Option::is_none")]
        bid_decimals: Option<u8>,
    },
    /// response indicating whether there any active bids
    HasBids { has_bids: bool },
    /// Viewing Key Error
    ViewingKeyError { error: String },
}

/// token's contract address and TokenInfo response
#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct Token {
    /// contract address of token
    pub contract_address: HumanAddr,
    /// Tokeninfo query response
    pub token_info: TokenInfo,
}

/// success or failure response
#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum ResponseStatus {
    Success,
    Failure,
}

/// Responses from handle functions
#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandleAnswer {
    /// response from consign attempt
    Consign {
        /// success or failure
        status: ResponseStatus,
        /// execution description
        message: String,
        /// amount consigned
        amount_consigned: Uint128,
        /// Optional amount that still needs to be consigned
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_needed: Option<Uint128>,
        /// Optional amount of tokens returned from escrow
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_returned: Option<Uint128>,
        /// decimal places for amounts
        sell_decimals: u8,
    },
    /// response from bid attempt
    Bid {
        /// success or failure
        status: ResponseStatus,
        /// execution description
        message: String,
        /// Optional amount of previous bid returned from escrow
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_bid: Option<Uint128>,
        /// Optional minimum bid amount
        #[serde(skip_serializing_if = "Option::is_none")]
        minimum_bid: Option<Uint128>,
        /// Optional amount bid
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_bid: Option<Uint128>,
        /// Optional amount of tokens returned from escrow
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_returned: Option<Uint128>,
        /// decimal places for bid amounts
        bid_decimals: u8,
    },
    /// response from closing the auction
    CloseAuction {
        /// success or failure
        status: ResponseStatus,
        /// execution description
        message: String,
        /// Optional amount of winning bid
        #[serde(skip_serializing_if = "Option::is_none")]
        winning_bid: Option<Uint128>,
        /// Optional number of bid token decimals if there was a winning bid
        #[serde(skip_serializing_if = "Option::is_none")]
        bid_decimals: Option<u8>,
        /// Optional amount of sell tokens transferred to auction closer
        #[serde(skip_serializing_if = "Option::is_none")]
        sell_tokens_received: Option<Uint128>,
        /// Optional decimal places for sell token
        #[serde(skip_serializing_if = "Option::is_none")]
        sell_decimals: Option<u8>,
        /// Optional amount of bid tokens transferred to auction closer
        #[serde(skip_serializing_if = "Option::is_none")]
        bid_tokens_received: Option<Uint128>,
    },
    /// response from attempt to retract bid
    RetractBid {
        /// success or failure
        status: ResponseStatus,
        /// execution description
        message: String,
        /// Optional amount of tokens returned from escrow
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_returned: Option<Uint128>,
        /// Optional decimal places for amount returned
        #[serde(skip_serializing_if = "Option::is_none")]
        bid_decimals: Option<u8>,
    },
    /// response from attempt to change minimum bid
    ChangeMinimumBid {
        /// success or failure
        status: ResponseStatus,
        /// new minimum bid
        minimum_bid: Uint128,
        /// decimal places for minimum bid
        bid_decimals: u8,
    },
}

/// code hash and address of a contract
#[derive(Serialize, Deserialize, JsonSchema, Clone, PartialEq, Debug)]
pub struct ContractInfo {
    /// contract's code hash string
    pub code_hash: String,
    /// contract's address
    pub address: HumanAddr,
}

impl ContractInfo {
    /// Returns a StdResult<CosmosMsg> used to execute Transfer
    ///
    /// # Arguments
    ///
    /// * `recipient` - address tokens are to be sent to
    /// * `amount` - Uint128 amount of tokens to send
    pub fn transfer_msg(&self, recipient: HumanAddr, amount: Uint128) -> StdResult<CosmosMsg> {
        transfer_msg(
            recipient,
            amount,
            None,
            BLOCK_SIZE,
            self.code_hash.clone(),
            self.address.clone(),
        )
    }

    /// Returns a StdResult<CosmosMsg> used to execute RegisterReceive
    ///
    /// # Arguments
    ///
    /// * `code_hash` - String holding code hash contract to be called when sent tokens
    pub fn register_receive_msg(&self, code_hash: String) -> StdResult<CosmosMsg> {
        register_receive_msg(
            code_hash,
            None,
            BLOCK_SIZE,
            self.code_hash.clone(),
            self.address.clone(),
        )
    }

    /// Returns a StdResult<TokenInfo> from performing TokenInfo query
    ///
    /// # Arguments
    ///
    /// * `querier` - a reference to the Querier dependency of the querying contract
    pub fn token_info_query<Q: Querier>(&self, querier: &Q) -> StdResult<TokenInfo> {
        token_info_query(
            querier,
            BLOCK_SIZE,
            self.code_hash.clone(),
            self.address.clone(),
        )
    }
}
