enum Address {
    Socket(NodeId, SocketId)
    Node(NodeId),
    Channel(NodeId, ChannelId)
}



// Node[Socket -> LocalChannel]

// NodeA[Socket -> RemoteChannel<PubSub>] <-> Network <-> NodeB[LocalChannel<PubSub>]
