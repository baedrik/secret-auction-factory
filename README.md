# Sealed Bid Auction Factory
This is an update to the [sealed-bid auction](https://github.com/baedrik/SCRT-sealed-bid-auction) secret contract running on Secret Network.  The new implementation incorporates the use of a factory contract to allow someone to view only the active auctions or only the closed auctions.  They may also view only the auctions that they have created, the auctions in which they have active bids, and the auctions they have won if they have created a viewing key with the factory contract.  If they have created a viewing key, the factory will automatically set the viewing key with every active auction they have bids with to allow the bidder to view their bid information directly from the auction.  Using a factory also enables the ability to consign sale tokens to the auction escrow in the same transaction that creates the auction, so we can now prevent any auctions from existing in a non-consigned state.

Although the original auction contract will no longer be updated, the repo will remain available so that people may use it as an example for writing secret contracts.  The original repo includes an in depth [WALKTHROUGH](https://github.com/baedrik/SCRT-sealed-bid-auction/blob/master/WALKTHROUGH.md) as well as an explanation of [CALLING_OTHER_CONTRACTS](https://github.com/baedrik/SCRT-sealed-bid-auction/blob/master/CALLING_OTHER_CONTRACTS.md).

## Creating a New Auction
First you must give the factory an allowance to consign the tokens for sale to the new auction's escrow:
```sh
secretcli tx compute execute *sale_tokens_contract_address* '{"increase_allowance":{"spender":"secret1vwagf5g3uap6hx2jdhsnzdar44czkz3rr0xxyr","amount":"*amount_being_sold_in_smallest_denomination_of_sale_token*"}}' --from *your_key_alias_or_addr* --gas 150000 -y
```
Granting allowance does not send any tokens, it only gives the factory permission to send tokens on your behalf. If for some reason after performing the above command you decide not to proceed with creating the auction, you may revoke that permission by using the `decrease_allowance` command. `decrease_allowance` follows the exact same input format as the above command.

Then you can create the auction with:
```sh
secretcli tx compute execute --label 253dot1 '{"create_auction":{"label":"*your_auction_name*","sell_contract":{"code_hash":"*sale_tokens_code_hash*","address":"*sale_tokens_contract_address*"},"bid_contract":{"code_hash":"*bid_tokens_code_hash*","address":"*bid_tokens_contract_address*"},"sell_amount":"*amount_being_sold_in_smallest_denomination_of_sale_token*","minimum_bid":"*minimum_accepted_bid_in_smallest_denomination_of_bid_token*","ends_at":*seconds_since_epoch_after_which_anyone_may_close_the_auction*,"description":"*optional_text_description*"}}' --from *your_key_alias_or_addr* --gas 600000 -y
```
You can find a contract's code hash with
```sh
secretcli q compute contract-hash *contract_address*
```
Copy it without the 0x prefix and surround it with quotes in the instantiate command.

When the factory creates the new auction it will send the tokens you are putting up for sale to the auction's escrow.  If you did not give the factory sufficient allowance, or if your token balance is less than the sale amount, the auction will not be created.

The `ends_at` time is represented in seconds since epoch 01/01/1970.  Before that time, only the auction creator can finalize the auction.  At that time or later, anyone may finalize the auction.  Bid will still be accepted after the `ends_at` time if no one has closed the auction yet.

The description field is optional.  It will accept a free-form text string (best to avoid using double-quotes).

The auction will not allow a sale amount of 0

The auction will not currently allow the sale contract address to be the same as the bid contract address, because there is no reason to swap different amounts of the same fungible token.  When the SNIP-721 spec is more fleshed out, this will probably be changed to allow for the exchanging of different NFT token IDs regardless of whether they are part of the same NFT contract or not.

## Changing the Minimum Bid
The seller of an auction may change the minimum bid at any time before the auction has closed:
```sh
secretcli tx compute execute *auction_contract_address* '{"change_minimum_bid": {"minimum_bid": "*minimum_accepted_bid_in_smallest_denomination_of_bid_token*"}}' --from *your_key_alias_or_addr* --gas 190000 -y
```
The new minimum bid will only apply to newly placed bids.  Any bids that were validly placed before the minimum bid was changed will remain valid.  For example, if Alice originally set the minimum bid to 5, and Bob placed a bid of 7 that is currently the highest bid, Bob's bid will remain valid even if Alice changes the minimum bid to 10.  If Charlie tries to place a bid of 8 after Alice has changed the minimum bid to 10, Charlie's bid will be rejected.  If no one places a bid that meets the new minimum, Bob's bid of 7 will win despite the minimum having changed after he placed his bid.

## Create a Viewing Key
You can have the factory generate a new viewing key with:
``` sh
secretcli tx compute execute --label 253dot1 '{"create_viewing_key":{"entropy":"*Some arbitrary string used as entropy in generating the random viewing key*"}}' --from *your_key_alias_or_addr* --gas 120000 -y
```
or you can set the viewing key with
``` sh
secretcli tx compute execute --label 253dot1 '{"set_viewing_key":{"key":"*The viewing key you want*","padding":"Optional string used to pad the message length so it does not leak the length of the key"}}' --from *your_key_alias_or_addr* --gas 120000 -y
```
A viewing key allows you to query the factory contract for a list of only the auctions you have interacted with.  It also allows you to view your bid information by querying the individual auctions.  It is recommended to use `create_viewing_key` to set your viewing key instead of using `set_viewing_key` because `create_viewing_key` will generate a complex key, whereas `set_viewing_key` will just accept whatever key it is given.  `set_viewing_key` is provided if a UI would like to generate the viewing key itself.  If you are developing a UI and are using `set_viewing_key`, also use the `padding` field so that the length of the message does not leak information about the length of the viewing key.

## Placing Bids
To place a bid, the bidder should Send the tokens to the individual auction contract address with
```sh
secretcli tx compute execute *bid_tokens_contract_address* '{"send": {"recipient": "*auction_contract_address*", "amount": "*bid_amount_in_smallest_denomination_of_bidding_token*"}}' --from *your_key_alias_or_addr* --gas 300000 -y
```
The tokens bid will be placed in escrow until the auction has concluded or you call retract\_bid to retract your bid and have all tokens returned.  You may retract your bid at any time before the auction ends. You may only have one active bid at a time.  If you place more than one bid, the smallest bid will be returned to you, because obviously that bid will lose to your new bid if they both stayed active.  If you bid the same amount as your previous bid, it will retain your original bid's timestamp, because, in the event of ties, the bid placed earlier is deemed the winner.  If you place a bid that is less than the minimum bid, those tokens will be immediately returned to you.  Also, if you place a bid after the auction has closed, those tokens will be immediately returned.

The auction will not allow a bid of 0.

It is recommended that the UI designed to send a bid use the optional "padding" field when calling the bid token contract's Send function.  You should make the number of digits of the bid amount + the number of characters in the "padding" field a constant.  That way the size of the Send does not leak information about the size of the bid.  The largest 128-bit unsigned integer has 39 digits, so the number of digits + the number of padding spaces should be at least 40.

## Retract Your Active Bid
You may retract your current active bid with
```sh
secretcli tx compute execute *auction_contract_address* '{"retract_bid": {}}' --from *your_key_alias_or_addr* --gas 300000 -y
```
You may retract your bid at any time before the auction closes to both retract your bid and to return your tokens.  In the unlikely event that your tokens were not returned automatically when the auction ended, you may call retract_bid after the auction closed to return them manually.

## Finalizing the Auction Sale
An auction may be closed with:
```sh
secretcli tx compute execute *auction_contract_address* '{"finalize": {"only_if_bids": *true_or_false*}}' --from *your_key_alias_or_addr* --gas 2000000 -y
```
Before the `ends_at` time is reached, only the auction creator can finalize an auction.  At that time or later, anyone may finalize the auction.  Even after the `ends_at` time is reached, bids will be accepted until the auction closes.

The boolean only\_if\_bids parameter is used to prevent the auction from closing if there are no active bids.  If there are no active bids, but only\_if\_bids was set to false, then the auction will be closed, and all consigned tokens will be returned to the auction creator.
If there is at least one active bid, the highest bid will be accepted (if tied, the tying bid placed earlier will be accepted).  The auction will then swap the tokens between the auction creator and the highest bidder, and return all the non-winning bids to their respective bidders.

## Returning Funds In The Event Of Error
In the unlikely event of some unforeseen error that results in funds being held by an auction after it has closed, anyone may run
```sh
secretcli tx compute execute *auction_contract_address* '{"return_all": {}}' --from *your_key_alias_or_addr* --gas 2000000 -y
```
Return_all may only be called after an auction is closed.  Auction\_info will indicate whether any funds are still held by a closed auction.  Even if return\_all is not called, bidders who have not received their bids back can still call retract\_bid to have their bids returned.

## View Lists of Active/Closed Auctions
You may view the list of active auctions sorted by pair with
```sh
secretcli q compute query secret1vwagf5g3uap6hx2jdhsnzdar44czkz3rr0xxyr '{"list_active_auctions":{}}'
```
You may view the list of closed auctions in reverse chronological order with
```sh
secretcli q compute query secret1vwagf5g3uap6hx2jdhsnzdar44czkz3rr0xxyr '{"list_closed_auctions":{"before":*optional_u64_timestamp*,"page_size":*optional_u32_number_to_list*}}'
```
If you do not supply the `before` field, it will start with the most recently closed auction, otherwise it will only display auctions that had closed before the provided timestamp.  The timestamp is in the number of seconds since epoch time (01/01/1970).  If you do not supply the `page_size` field, it will default to listing up to 200 closed auctions, otherwise it will display up to the number specifed as `page_size`.

If you are paginating your list, you would take the timestamp of the last auction returned, and specify that in the `before` field of the next query in order to have the subsequent query start where the last one left off.

## View List of Your Auctions
You may view the lists of auctions that you have created, in which you have an active bid, or you have won with
```sh
secretcli q compute query secret1vwagf5g3uap6hx2jdhsnzdar44czkz3rr0xxyr '{"list_my_auctions":{"address":"*address_whose_auctions_to_list*","viewing_key":"*viewing_key*","filter":"*optional choice of active, closed, or all*"}}'
```
To view your own auctions, you will need to have created a viewing key with the factory contract.  The `filter` field is an optional field that can be "active", "closed", or "all", to list only active , closed, or all your auctions respectively.  If you do not specify a filter, it will list all your auctions.

## Viewing an Individual Auction's Information
You can view the sell and bid token information, the amount being sold, the minimum bid, the closing time, the description if present, the auction contract address, and the status of the auction with
```sh
secretcli q compute query *auction_contract_address* '{"auction_info":{}}'
```
Status will either be "Closed" if the auction is over, or it will be "Accepting bids".  If the auction is closed, it will also display the winning bid if there was one.

If the auction is closed, it will display if there are any outstanding funds still residing in the auction account.  This should never happen, but if it does for some unforeseen reason, it will remind the user to either use retract\_bid to have their bid tokens returned (if they haven't already been returned), or use return\_all to return all the funds still held by the auction.  Return\_all can only be called after the auction has closed.

## View Your Active Bid in an Individual Auction
You may view your current active bid amount and the time the bid was placed with
```sh
secretcli q compute query *auction_contract_address* '{"view_bid": {"address":"*address_whose_bid_to_list*","viewing_key":"*viewing_key*"}}'
```
You must have created a viewing key with the factory contract before you can view an active bid in an auction.

## Notes for UI builders
It is recommended that the UI designed to send a bid use the optional "padding" field when calling the bid token contract's Send function.  You will want the number of digits of the send amount + the number of characters in the padding field to be a constant number (I use 40 characters, because the maximum number of digits of Uint128 is 39, and I always want at least one blank in padding).  That way the size of the Send does not leak information about the size of the bid.

It would be preferred if the UI performed the allowance-granting step and the creation of the auction as one process for the user.  That is, when the user selects to create an auction, the UI should first grant the allowance and then immediately create the auction.  Because they are two separate transactions, the user will need to confirm each one, so you might want to display the steps to inform the user of what they are confirming, but the steps should automatically follow each other.

Don't forget that secret contracts can not use floating points, so any time a token/auction has an amount input/output, it is an integer in the smallest denomination of the token.  Therefore it is up to the UI to convert input/output amounts from/to their decimal equivalent to make things more convenient for the user.  So if your UI accepts a decimal input from the user, it will need to multiply the input by 10^number_of_decimals before creating the execute commands.  That means that when creating a new auction, your UI will need to query the input sell and bid contract addresses to find out how many decimal places they use so that it can translate the sell_amount and minimum_bid to the integer form that the auction and token contracts use.  When placing a bid, if your UI is already pulling the auction_info data when it displays the auction the user wants to bid in, you can just pull the decimal places from the auction_info query instead of needing to query the individual token contracts.  The same applies to responses the UI receives from the auction.  Any amounts will need to be divided by 10^n_decimals if you are going to display amounts in decimal values of whole tokens.  Again, since the UI will likely have already called the auction_info query to display it before the user interacts with the auction, it can store the number of decimals from the auction_info query to use later in translating the response.  When displaying lists of active/closed auctions, the factory will supply the number of decimals (in fields called `sell_decimals` and `bid_decimals`) so that the UI doesn't have to query every auction in the list.

If you are paginating your list of closed auctions, you would take the timestamp of the last auction returned, and specify that in the `before` field of the next query in order to have the subsequent query start where the last one left off.

Also, you should be aware that responses from bidding and consigning (functions that are called indirectly when doing a Send tx with a token contract) are sent in the log attributes.  Also, the address of a newly created auction is returned in a log attribute.  This is because when one contract calls another contract, only logs (not the data field) are forwarded back to the user.  On the other hand, any time you call a contract directly that does not need to call another contract (or that can ignore the other contract's response), the response will be sent in the data field, which is the preferred method of returning json responses.