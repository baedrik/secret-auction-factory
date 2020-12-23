# Sealed Bid Auction Factory
I'm updating the sealed bid auction so that auctions are spawned by a factory contract.  This enables you to do queries to just list the active auctions (or just the closed auctions), as well as list only the auctions you are the seller in, have active bids in, or won.  Also, you will set your viewing key with the factory, and it automatically keeps all auctions you have active bids with updated with that viewing key so you only need one for all of them.

There is still a lot of testing I want to do, but I thought I'd put it out there early so anyone that is building a UI, can get a feel for what has changed.  I still need to implement two queries with the factory, ListActive, and ListClosed to list the active and closed auctions respectively.  I am planning to have ListClosed accept an optional parameter for how many auctions you want returned as well as an optional parameter for what timestamp you want to start listing from.  Closed auctions are returned in reverse chronological order, so if you are paginating, you would read the timestamp of the last auction you received, and use that as the timestamp in your subsequent query and it will start where it left off. 