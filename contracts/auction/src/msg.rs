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
        /// true if auction creator wants to keep the auction open if there are no active bids
        only_if_bids: bool,
    },

    /// If the auction holds any funds after it has closed (should never happen), this will return
    /// those funds to their owners.  Should never be needed, but included in case of unforeseen
    /// error
    ReturnAll {},

    ///  SetViewingkey can only be called by the factory contract to ensure all auctions use the
    ///  same viewing key for one user
    SetViewingKey {
        /// set key for this addres
        bidder: HumanAddr,
        /// viewing key
        key: String,
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
        address: HumanAddr,
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
    },
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
        /// Optional amount consigned
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_consigned: Option<Uint128>,
        /// Optional amount that still needs to be consigned
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_needed: Option<Uint128>,
        /// Optional amount of tokens returned from escrow
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_returned: Option<Uint128>,
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
        /// Optional amount of tokens returned form escrow
        #[serde(skip_serializing_if = "Option::is_none")]
        amount_returned: Option<Uint128>,
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
    },
    /// generic status response
    Status {
        /// success or failure
        status: ResponseStatus,
        /// execution description
        message: String,
    },
}

/// code hash and address of a contract
#[derive(Serialize, Deserialize, JsonSchema, Clone)]
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
