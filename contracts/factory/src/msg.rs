use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use cosmwasm_std::{HumanAddr, Uint128};

/// Instantiation message
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct InitMsg {
    /// entropy used to generate prng seed
    pub entropy: String,
    /// auction contract info
    pub auction_contract: AuctionContractInfo,
}

/// Handle messages
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandleMsg {
    /// CreateAuction will instantiate a new auction
    CreateAuction {
        /// String label for the auction
        label: String,
        /// sell contract code hash and address
        sell_contract: ContractInfo,
        /// bid contract code hash and address
        bid_contract: ContractInfo,
        /// amount of tokens being sold
        sell_amount: Uint128,
        /// minimum bid that will be accepted
        minimum_bid: Uint128,
        /// Optional free-form description of the auction (best to avoid double quotes). As an example
        /// it could be the date the owner will likely finalize the auction, or a list of other
        /// auctions for the same token, etc...
        #[serde(default)]
        description: Option<String>,
    },

    /// RegisterAuction saves the auction info of a newly instantiated auction and adds it to the list
    /// of active auctions as well as adding it to the seller's list of auctions
    ///
    /// Only auctions will use this function
    RegisterAuction {
        /// seller/creator of the auction
        seller: HumanAddr,
        /// auction information needed by the factory
        auction: RegisterAuctionInfo,
    },

    /// CloseAuction tells the factory that the auction closed and provides the winning bid if appropriate
    ///
    /// Only auctions will use this function
    CloseAuction {
        /// auction seller
        seller: HumanAddr,
        /// winning bidder if the auction ended in a swap
        #[serde(default)]
        bidder: Option<HumanAddr>,
        /// winning bid if the auction ended in a swap
        #[serde(default)]
        winning_bid: Option<Uint128>,
    },

    /// RegisterBidder allows the factory to know an auction has a new bidder so it can update their
    /// list of auctions, as well a create a viewing key for the auction if one was set
    ///
    /// Only auctions will use this function    
    RegisterBidder { bidder: HumanAddr },

    /// RemoveBidder allows the factory to know a bidder retracted his bid from an auction
    ///
    /// Only auctions will use this function    
    RemoveBidder { bidder: HumanAddr },

    /// Allows the admin to add a new auction contract version
    NewAuctionContract {
        auction_contract: AuctionContractInfo,
    },

    /// Create a viewing key to be used with all factory and auction authenticated queries
    CreateViewingKey { entropy: String },

    /// Set a viewing key to be used with all factory and auction authenticated queries
    SetViewingKey {
        key: String,
        // optional padding can be used so message length doesn't betray key length
        padding: Option<String>,
    },

    /// Allows an admin to start/stop all auction creation
    SetStatus { stop: bool },
}

/// Queries
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    /// lists all auctions the given address has owned, won, or have an active bid
    ListMyAuctions {
        // address whose activity to display
        address: HumanAddr,
        /// viewing key
        viewing_key: String,
        /// optional filter for only active or closed auctions.  If not specified, lists all
        #[serde(default)]
        filter: Option<FilterTypes>,
    },
    /// lists all active auctions sorted by pair
    ListActiveAuctions {},
    /// lists closed auctions in reverse chronological order.  If you specify page size, it return
    /// only that number of auctions (default is 200).  If you specify the before timestamp, it will
    /// start listing from the first auction that closed before the timestamp (in number of seconds
    /// since epoch 01/01/1970).  If you are paginating, you would take the timestamp of the last
    /// auction you receive, and specify that as the before timestamp on your next query so it will
    /// continue where it left off
    ListClosedAuctions {
        /// optionally only show auctions that closed before this timestamp (number of seconds from
        /// epoch 01/01/1970)
        #[serde(default)]
        before: Option<u64>,
        /// optional number of auctions to return
        #[serde(default)]
        page_size: Option<u32>,
    },
}

/// the filter types when viewing an address' auctions
#[derive(Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FilterTypes {
    Active,
    Closed,
    All,
}

/// responses to queries
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryAnswer {
    /// List the auctions where address is either the seller of bidder (or won)
    ListMyAuctions {
        /// lists of the address' active auctions
        #[serde(skip_serializing_if = "Option::is_none")]
        active: Option<MyActiveLists>,
        /// lists of the address' closed auctions
        #[serde(skip_serializing_if = "Option::is_none")]
        closed: Option<MyClosedLists>,
    },
    /// List active auctions sorted by pair
    ListActiveAuctions {
        /// active auctions sorted by pair
        #[serde(skip_serializing_if = "Option::is_none")]
        active: Option<Vec<AuctionInfo>>,
    },
    /// List closed auctions in reverse chronological order
    ListClosedAuctions {
        /// closed auctions in reverse chronological order
        #[serde(skip_serializing_if = "Option::is_none")]
        closed: Option<Vec<ClosedAuctionInfo>>,
    },
    /// Viewing Key Error
    ViewingKeyError { error: String },
}

/// Lists of active auctions sorted by pair where the address is a seller or bidder
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MyActiveLists {
    /// active auctions sorted by pair where the address is the seller
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_seller: Option<Vec<AuctionInfo>>,
    /// active auctions sorted by pair where the address is the bidder
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_bidder: Option<Vec<AuctionInfo>>,
}

/// Lists of closed auctions in reverse chronological order where the address is a
/// seller or won
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MyClosedLists {
    /// closed auctions in reverse chronological order where the address is the seller
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_seller: Option<Vec<ClosedAuctionInfo>>,
    /// closed auctions in reverse chronological order where the address won
    #[serde(skip_serializing_if = "Option::is_none")]
    pub won: Option<Vec<ClosedAuctionInfo>>,
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
    /// response from creating a viewing key
    ViewingKey { key: String },
    /// generic status response
    Status {
        /// success or failure
        status: ResponseStatus,
        /// execution description
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

/// code hash and address of a contract
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ContractInfo {
    /// contract's code hash string
    pub code_hash: String,
    /// contract's address
    pub address: HumanAddr,
}

/// Info needed to instantiate an auction
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct AuctionContractInfo {
    /// code id of the stored auction contract
    pub code_id: u64,
    /// code hash of the stored auction contract
    pub code_hash: String,
}

/// active auction display info
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct AuctionInfo {
    /// auction address
    pub address: HumanAddr,
    /// auction label
    pub label: String,
    /// symbols of tokens for sale and being bid in form of SELL-BID
    pub pair: String,
    /// sell amount
    pub sell_amount: Uint128,
    /// minimum bid
    pub minimum_bid: Uint128,
}

/// active auction info for storage
#[derive(Serialize, Deserialize, JsonSchema, Debug)]
pub struct RegisterAuctionInfo {
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
    /// auction contract version number
    pub version: u8,
}

impl RegisterAuctionInfo {
    /// takes the register auction information and creates a store auction info struct
    pub fn to_store_auction_info(&self) -> StoreAuctionInfo {
        StoreAuctionInfo {
            label: self.label.clone(),
            sell_symbol: self.sell_symbol,
            bid_symbol: self.bid_symbol,
            sell_amount: self.sell_amount.u128(),
            minimum_bid: self.minimum_bid.u128(),
            version: self.version,
        }
    }
}

/// active auction info for storage
#[derive(Serialize, Deserialize, JsonSchema, Debug)]
pub struct StoreAuctionInfo {
    /// auction label
    pub label: String,
    /// sell symbol index
    pub sell_symbol: u16,
    /// bid symbol index
    pub bid_symbol: u16,
    /// sell amount
    pub sell_amount: u128,
    /// minimum bid
    pub minimum_bid: u128,
    /// auction contract version number
    pub version: u8,
}

impl StoreAuctionInfo {
    /// takes the active auction information and creates a closed auction info struct
    pub fn to_store_closed_auction_info(
        &self,
        winning_bid: Option<u128>,
        timestamp: u64,
    ) -> StoreClosedAuctionInfo {
        StoreClosedAuctionInfo {
            label: self.label.clone(),
            sell_symbol: self.sell_symbol,
            bid_symbol: self.bid_symbol,
            sell_amount: self.sell_amount,
            winning_bid,
            timestamp,
        }
    }
}

/// closed auction display info
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ClosedAuctionInfo {
    /// auction address
    pub address: HumanAddr,
    /// auction label
    pub label: String,
    /// symbols of tokens for sale and being bid in form of SELL-BID
    pub pair: String,
    /// sell amount
    pub sell_amount: Uint128,
    /// winning bid
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winning_bid: Option<Uint128>,
    /// time the auction closed in seconds since epoch 01/01/1970
    pub timestamp: u64,
}

/// closed auction storage format
#[derive(Serialize, Deserialize)]
pub struct StoreClosedAuctionInfo {
    /// auction label
    pub label: String,
    /// sell symbol index
    pub sell_symbol: u16,
    /// bid symbol index
    pub bid_symbol: u16,
    /// sell amount
    pub sell_amount: u128,
    /// winning bid
    pub winning_bid: Option<u128>,
    /// time the auction closed in seconds since epoch 01/01/1970
    pub timestamp: u64,
}
