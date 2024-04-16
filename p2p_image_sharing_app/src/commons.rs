use crate::fragment;
use fragment::{Fragment, Image};
use serde_derive::{Deserialize, Serialize};
use std::{collections::HashMap, net::SocketAddr};

pub const BUFFER_SIZE: usize = 32768;
pub const FRAG_SIZE: usize = 8000;
pub const BLOCK_SIZE: usize = 8;
pub const TIMEOUT_MILLIS: usize = 2000;
pub const SERVICE_PORT: usize = 8080;
pub const ELECTION_PORT: usize = 8081;
pub const SERVICE_SENDBACK_PORT: usize = 8082;
pub const SERVERS_FILEPATH: &str = "./servers.txt";
pub const REQ_ID_LOG_FILEPATH: &str = "./req_id_log.txt";
pub const PICS_ROOT_PATH: &str = "./pics";
pub const HIGH_RES_PICS_PATH: &str = "./pics/high";
pub const LOW_RES_PICS_PATH: &str = "./pics/low";
pub const ENCRYPTED_PICS_PATH: &str = "./pics/encrypted";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Msg {
    pub sender: SocketAddr,
    pub receiver: SocketAddr,
    pub msg_type: Type,
    pub payload: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Type {
    ClientRequest(u32),
    ElectionRequest(f32),
    OKMsg(f32),
    CoordinatorBrdCast(String),
    Ack(String, u32),
    Fragment(Fragment),
    Fail(u32),
    DirOfServQuery,
    DirOfServQueryReply(HashMap<SocketAddr, bool>),
    ClientDirOfServQueryPending,
    ClientDirOfServQueryPendingReply(Option<HashMap<String, Action>>),
    ServerDirOfServQueryPending,
    ServerDirOfServQueryPendingReply(HashMap<SocketAddr, HashMap<String, Action>>),
    DirOfServJoin,
    DirOfServLeave,
    LowResImgReq,
    LowResImgReply(Fragment),
    ImageRequest(String, u32),
    SharedImage(String, Image, u32),
    UpdateAccessRequest(String, Action),
    UpdateAccess(String, Action),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Action {
    Increment(u32),
    Decrement(u32),
    Revoke,
}
