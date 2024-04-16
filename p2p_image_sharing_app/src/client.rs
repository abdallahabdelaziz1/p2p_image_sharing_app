#![allow(
    dead_code,
    unused_variables,
    unused_imports,
    clippy::redundant_allocation,
    unused_assignments
)]

extern crate serde;
extern crate serde_derive;
extern crate serde_json;
use crate::commons::{
    self, Action, ENCRYPTED_PICS_PATH, HIGH_RES_PICS_PATH, LOW_RES_PICS_PATH, PICS_ROOT_PATH,
    REQ_ID_LOG_FILEPATH,
};
use crate::dir_of_service::ClientDirOfService;
use crate::encryption::{decode_img, encode_img};
use crate::fragment::{self, BigMessage};
use crate::utils::{
    create_output_dirs, file_exists, get_cloud_servers, get_pic_paths, get_req_id_log, mkdir,
};
use commons::{Msg, Type};
use commons::{BUFFER_SIZE, ELECTION_PORT, SERVERS_FILEPATH, SERVICE_PORT, SERVICE_SENDBACK_PORT};
use fragment::Image;
use image::{open, ImageBuffer, Rgba};
use log::{error, info, log, trace, warn};
use minifb::{Key, Window, WindowOptions};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::{env, fs as std_fs};
use tokio::io::AsyncBufReadExt;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio::{self, fs};

pub struct ClientBackend {
    cloud_socket: Arc<UdpSocket>,
    pub client_socket: Arc<UdpSocket>,
    next_req_id: u32,
    mode: String,
    cloud_servers: Vec<(SocketAddr, SocketAddr)>,
    dir_of_serv: ClientDirOfService,
    received_complete_imgs: HashMap<String, BigMessage>,
    pub own_shared_imgs: Arc<Mutex<HashMap<SocketAddr, Vec<(String, u32)>>>>,
    pub received_shared_imgs: Arc<Mutex<HashMap<SocketAddr, Vec<(String, u32)>>>>,
    pub requests: Arc<Mutex<HashMap<String, Action>>>,
    pub low_res_imgs_tmp: Arc<Mutex<Vec<String>>>,
}

impl ClientBackend {
    pub async fn new(args: Vec<String>) -> ClientBackend {
        let mode = args[1].as_str();
        let ip_to_cloud: SocketAddr = args[2]
            .as_str()
            .parse()
            .expect("Failed to parse IP from input");

        let ip_to_clients: SocketAddr = SocketAddr::new(ip_to_cloud.ip(), ip_to_cloud.port() + 1);
        let next_req_id = get_req_id_log(REQ_ID_LOG_FILEPATH);

        let cloud_socket: Arc<UdpSocket> = Arc::new(
            UdpSocket::bind(ip_to_cloud)
                .await
                .expect("Failed to bind to ip"),
        );
        info!("Cloud Communication on {ip_to_cloud}");

        let client_socket = Arc::new(
            UdpSocket::bind(ip_to_clients)
                .await
                .expect("Failed to bind to ip"),
        );
        info!("Clients Communication on {ip_to_clients}");

        let cloud_servers = get_cloud_servers(SERVERS_FILEPATH, mode);

        ClientBackend {
            cloud_socket,
            client_socket,
            next_req_id,
            mode: String::from(mode),
            cloud_servers,
            dir_of_serv: ClientDirOfService::new(),
            received_complete_imgs: HashMap::new(),
            own_shared_imgs: Arc::new(Mutex::new(HashMap::new())),
            received_shared_imgs: Arc::new(Mutex::new(HashMap::new())),
            requests: Arc::new(Mutex::new(HashMap::new())),
            low_res_imgs_tmp: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn init(&self) {
        let mut clients_buffer: [u8; BUFFER_SIZE] = [0; BUFFER_SIZE];
        let client_socket = self.client_socket.clone();
        let own_shared_imgs = self.own_shared_imgs.clone();
        let received_shared_imgs = self.received_shared_imgs.clone();
        let requests = self.requests.clone();
        let low_res_img_tmp = self.low_res_imgs_tmp.clone();
        ClientDirOfService::join(self.cloud_socket.clone(), self.cloud_servers.clone()).await;
        let pending_updates = self.query_pending_updates().await;
        if let Some(actions_map) = pending_updates {
            info!("Pending Updates: {:?}", actions_map);
            for (key, value) in actions_map {
                let partial_img_id_parts: Vec<&str> = key.split('&').collect();
                let img_id = String::from(partial_img_id_parts[0])
                    + "&"
                    + self
                        .client_socket
                        .local_addr()
                        .unwrap()
                        .to_string()
                        .as_str()
                    + "&"
                    + partial_img_id_parts[1];
                let src_addr = partial_img_id_parts[0].parse::<SocketAddr>().unwrap();
                ClientBackend::handle_update_access(
                    img_id,
                    value.clone(),
                    src_addr,
                    received_shared_imgs.clone(),
                )
                .await;
            }
        }
        let mut received_complete_imgs: HashMap<String, BigMessage> = HashMap::new();

        let h1 = tokio::spawn({
            async move {
                loop {
                    match client_socket.recv_from(&mut clients_buffer).await {
                        Ok((bytes_read, src_addr)) => {
                            info!("{} bytes from {}.", bytes_read, src_addr);

                            let msg: Msg =
                                serde_cbor::de::from_slice(&clients_buffer[..bytes_read])
                                    .expect("Failed to deserialize msg from client socket");

                            ClientBackend::handle_msg_from_client(
                                client_socket.clone(),
                                msg,
                                src_addr,
                                &mut received_complete_imgs,
                                own_shared_imgs.clone(),
                                received_shared_imgs.clone(),
                                requests.clone(),
                                low_res_img_tmp.clone(),
                            )
                            .await;
                        }
                        Err(e) => {
                            error!("Error receiving data from client socket: {}", e);
                        }
                    }
                }
            }
        });
    }

    async fn handle_msg_from_client(
        client_socket: Arc<UdpSocket>,
        msg: Msg,
        src_addr: SocketAddr,
        received_complete_imgs: &mut HashMap<String, BigMessage>,
        own_shared_imgs: Arc<Mutex<HashMap<SocketAddr, Vec<(String, u32)>>>>,
        received_shared_imgs: Arc<Mutex<HashMap<SocketAddr, Vec<(String, u32)>>>>,
        requests: Arc<Mutex<HashMap<String, Action>>>,
        low_res_img_tmp: Arc<Mutex<Vec<String>>>,
    ) {
        match msg.msg_type {
            Type::LowResImgReq => {
                ClientBackend::handle_low_res_imgs_req(client_socket.clone(), src_addr).await;
            }

            Type::LowResImgReply(frag) => {
                ClientBackend::handle_low_res_imgs_reply(
                    client_socket,
                    frag,
                    src_addr,
                    received_complete_imgs,
                    low_res_img_tmp,
                )
                .await;
            }

            Type::ImageRequest(img_name, requested_access) => {
                ClientBackend::handle_image_request(
                    img_name,
                    requested_access,
                    client_socket.clone(),
                    src_addr,
                    own_shared_imgs,
                )
                .await;
            }

            Type::Fragment(frag) => {
                if let Some(req_id) = fragment::receive_one(
                    client_socket.clone(),
                    frag,
                    src_addr,
                    received_complete_imgs,
                )
                .await
                {
                    let data = received_complete_imgs.get(&req_id).unwrap().data.to_owned();
                    received_complete_imgs.remove(&req_id);

                    if let Ok(Msg {
                        msg_type: Type::SharedImage(img_id, img, recieved_access),
                        ..
                    }) = serde_cbor::de::from_slice(&data)
                    {
                        ClientBackend::handle_shared_image(
                            req_id,
                            img,
                            recieved_access,
                            client_socket.clone(),
                            src_addr,
                            received_shared_imgs,
                        )
                        .await;
                    }
                }
            }

            // Type::SharedImage(img_id, img, recieved_access) => {
            //     ClientBackend::handle_shared_image(
            //         img_id,
            //         img,
            //         recieved_access,
            //         client_socket.clone(),
            //         src_addr,
            //     )
            //     .await;
            // }
            Type::UpdateAccessRequest(img_id, action) => {
                requests.lock().await.insert(img_id, action);
                // ClientBackend::handle_update_access_req(
                //     img_id,
                //     action,
                //     client_socket.clone(),
                //     src_addr,
                // )
                // .await;
            }

            Type::UpdateAccess(img_id, action) => {
                ClientBackend::handle_update_access(img_id, action, src_addr, received_shared_imgs)
                    .await;
            }
            _ => {}
        }
    }

    pub async fn request_low_res_images(&self, client_addr: SocketAddr) {
        info!("Send Low Res Image Request");
        let msg = Msg {
            sender: self.client_socket.local_addr().unwrap(),
            receiver: client_addr,
            msg_type: Type::LowResImgReq,
            payload: None,
        };
        let serialized_msg = serde_cbor::ser::to_vec(&msg).unwrap();
        self.client_socket
            .send_to(&serialized_msg, client_addr)
            .await
            .unwrap();
    }

    async fn handle_low_res_imgs_req(client_socket: Arc<UdpSocket>, src_addr: SocketAddr) {
        let pics_file_path: &str = "./pics1.txt";
        let pics = get_pic_paths(pics_file_path);

        for pic in &pics {
            let path = format!("{}/{}", LOW_RES_PICS_PATH, pic);
            trace!("{}", path);

            let pic_bin = fs::read(path).await.unwrap();
            // src unique
            let pic_id = format!("{}&{}", client_socket.local_addr().unwrap(), pic);
            fragment::client_send(
                pic_bin,
                client_socket.clone(),
                src_addr.to_string().as_str(),
                pic_id.as_str(),
                true,
            )
            .await;
        }
        info!("Finished sending low res pics");
    }

    // send all images in low folder
    // async fn handle_low_res_imgs_req(client_socket: Arc<UdpSocket>, src_addr: SocketAddr) {

    //     if let Ok(pics) = std::fs::read_dir(LOW_RES_PICS_PATH) {
    //         for pic in pics.flatten() {
    //             let pic_name = pic.file_name().to_string_lossy().into_owned();
    //             let path = format!("{}/{}", LOW_RES_PICS_PATH, pic_name);
    //             trace!("{}", path);
    //             let pic_bin = fs::read(path).await.unwrap();
    //             // src unique
    //             let pic_id = format!("{}&{}", client_socket.local_addr().unwrap(), pic_name);
    //             fragment::client_send(
    //                 pic_bin,
    //                 client_socket.clone(),
    //                 src_addr.to_string().as_str(),
    //                 pic_id.as_str(),
    //                 true,
    //             )
    //             .await;
    //         }
    //     }
    //     info!("Finished sending low res pics");
    // }

    async fn handle_low_res_imgs_reply(
        client_socket: Arc<UdpSocket>,
        frag: fragment::Fragment,
        src_addr: SocketAddr,
        received_complete_imgs: &mut HashMap<String, BigMessage>,
        low_res_img_tmp: Arc<Mutex<Vec<String>>>,
    ) {
        if let Some(pic_id) = fragment::receive_one(
            client_socket.clone(),
            frag,
            src_addr,
            received_complete_imgs,
        )
        .await
        {
            let data = received_complete_imgs.get(&pic_id).unwrap().data.to_owned();
            received_complete_imgs.remove(&pic_id);
            let path = format!("{}/{}", LOW_RES_PICS_PATH, src_addr);
            let parts: Vec<&str> = pic_id.split('&').collect();
            let pic_name = *parts.last().unwrap();
            println!("Received low res img ({}) from {}", pic_name, src_addr);
            mkdir(path.as_str());
            fs::write(format!("{}/{}", path, pic_name), data)
                .await
                .unwrap();
            low_res_img_tmp.lock().await.push(pic_name.to_string());
        }
    }

    async fn send_init_request_to_cloud(&self, t: Type) -> Option<SocketAddr> {
        let socket = self.cloud_socket.clone();
        let mode = self.mode.clone();

        let servers = self.cloud_servers.clone();
        let mut buffer = [0; 1024];

        for server in &servers {
            let target_addr = server.1;

            info!("Sending to server {:?}", server);
            let msg = Msg {
                sender: socket.local_addr().unwrap(),
                receiver: target_addr,
                msg_type: t.clone(),
                payload: None,
            };
            let serialized_msg = serde_cbor::ser::to_vec(&msg).unwrap();
            // let serialized_msg = serde_json::to_string(&msg).unwrap();
            socket.send_to(&serialized_msg, target_addr).await.unwrap();
        }

        let sleep = sleep(Duration::from_millis(5000));
        tokio::pin!(sleep);

        for _ in 0..10 {
            tokio::select! {
                    _ = &mut sleep => {
                        continue;
                    },
                    recv_result = socket.recv_from(&mut buffer) => {
                        match recv_result {
                            Ok((_bytes_read, src_addr)) => {
                                if let Ok(response) = std::str::from_utf8(&buffer[.._bytes_read]) {
                                    let chosen_server: SocketAddr = response.parse().unwrap();
                                    info!("{}", chosen_server);
                                    return Some(chosen_server);
                                } else {
                                    continue;
                                }
                            }
                            Err(e) => {
                                error!("Error getting the result of eletion: {}", e);
                            }
                    }
                }
            }
        }
        None
    }

    pub async fn send_image_request(
        &self,
        img_name: String,
        requested_access: u32,
        peer_client_addr: SocketAddr,
    ) {
        println!("Send Image Request");
        let msg = Msg {
            sender: self.client_socket.local_addr().unwrap(),
            receiver: peer_client_addr,
            msg_type: Type::ImageRequest(img_name, requested_access),
            payload: None,
        };
        let serialized_msg = serde_cbor::ser::to_vec(&msg).unwrap();
        self.client_socket
            .send_to(&serialized_msg, peer_client_addr)
            .await
            .unwrap();
    }

    async fn handle_image_request(
        img_name: String,
        requested_access: u32,
        client_socket: Arc<UdpSocket>,
        src_addr: SocketAddr,
        own_shared_imgs: Arc<Mutex<HashMap<SocketAddr, Vec<(String, u32)>>>>,
    ) {
        println!("Handle Image Request");
        let img_parts: Vec<&str> = img_name.split('.').collect();
        let path = format!("{}/{}.png", ENCRYPTED_PICS_PATH, img_parts.first().unwrap());
        if file_exists(path.as_str()) {
            let img_buffer = file_as_image_buffer(path);

            //Embedding the access limit in the image
            let mut img_buffer_with_access = img_buffer.clone();
            let (width, height) = img_buffer_with_access.dimensions();
            let access_pixel = img_buffer_with_access.get_pixel_mut(width - 1, height - 1);
            access_pixel[3] = requested_access as u8; //set the alpha channel of the last pixel to the access limit

            let image = Image {
                dims: img_buffer_with_access.dimensions(),
                data: img_buffer_with_access.into_raw(),
            };

            let msg = Msg {
                sender: client_socket.local_addr().unwrap(),
                receiver: src_addr,
                msg_type: Type::SharedImage(img_name.clone(), image, requested_access),
                payload: None,
            };

            let serialized_msg = serde_cbor::ser::to_vec(&msg).unwrap();
            let pic_id = format!(
                "{}&{}&{}.png",
                client_socket.local_addr().unwrap(),
                src_addr,
                img_parts.first().unwrap()
            );

            fragment::client_send(
                serialized_msg,
                client_socket.clone(),
                src_addr.to_string().as_str(),
                pic_id.as_str(),
                false,
            )
            .await;
            println!("Finished sending pic");

            let mut guard = own_shared_imgs.lock().await;
            let entry = guard.entry(src_addr).or_insert(Vec::new());
            if let Some(index) = entry.iter().position(|(s, _)| s == &pic_id) {
                entry[index] = (pic_id, requested_access);
            } else {
                entry.push((pic_id, requested_access));
            }
        } else {
            println!("File does not exist: {}", path);
        }
    }

    async fn handle_shared_image(
        pic_id: String,
        img: Image,
        recieved_access: u32,
        client_socket: Arc<UdpSocket>,
        src_addr: SocketAddr,
        received_shared_imgs: Arc<Mutex<HashMap<SocketAddr, Vec<(String, u32)>>>>,
    ) {
        println!("Handle Share Image");
        println!("Image ID: {}", pic_id);
        println!("Image Access: {}", recieved_access);
        let path_encrypted = format!("{}/{}", ENCRYPTED_PICS_PATH, src_addr);
        let path_decoded = format!("{}/{}", HIGH_RES_PICS_PATH, src_addr);
        let parts: Vec<&str> = pic_id.split('&').collect();
        let pic_name = *parts.last().unwrap();
        let pic_without_ext = pic_name.split('.').collect::<Vec<&str>>()[0];
        mkdir(path_encrypted.as_str());
        // mkdir(path_decoded.as_str());

        let image_buffer: image::ImageBuffer<Rgba<u8>, Vec<u8>> =
            image::ImageBuffer::from_raw(img.dims.0, img.dims.1, img.data).unwrap();

        save_image_buffer(
            image_buffer.clone(),
            format!("{}/{}", path_encrypted, pic_name),
        );

        let mut guard = received_shared_imgs.lock().await;
        let entry = guard.entry(src_addr).or_insert(Vec::new());
        if let Some(index) = entry.iter().position(|(s, _)| s == &pic_id) {
            entry[index] = (pic_id, recieved_access);
        } else {
            entry.push((pic_id, recieved_access));
        }
    }

    pub async fn view_image(&self, img_name: String, src_addr: SocketAddr) -> bool {
        let path = format!("{}/{}/{}", ENCRYPTED_PICS_PATH, src_addr, img_name);
        let mut img_buffer = file_as_image_buffer(path.clone());
        let (width, height) = img_buffer.dimensions();
        let access_pixel = img_buffer.get_pixel_mut(width - 1, height - 1);
        let access = access_pixel[3];
        println!("Access: {}", access);

        //Check if there is still access to the image
        if access == 0 {
            return false;
        }

        access_pixel[3] -= 1;

        let img_id = format!(
            "{}&{}&{}",
            src_addr,
            self.client_socket.local_addr().unwrap(),
            img_name
        );
        println!("Before");
        let mut guard = self.received_shared_imgs.lock().await;
        println!("After");
        let new_access = access_pixel[3];
        let entry = guard.entry(src_addr).or_insert(Vec::new());
        if let Some(index) = entry.iter().position(|(s, _)| s == &img_id) {
            entry[index] = (img_id, new_access as u32);
        } else {
            entry.push((img_id, new_access as u32));
        }
        drop(guard);

        let secret_bytes = decode_img(img_buffer.clone()).await;
        let decoded_buffer = image::load_from_memory(&secret_bytes).unwrap();
        let decoded_buffer = decoded_buffer.to_rgba8();
        let (width_decoded, height_decoded) = decoded_buffer.dimensions();

        save_image_buffer(img_buffer, path.clone());

        let name_without_ext = img_name.split('.').collect::<Vec<&str>>()[0];
        // Set the title and size of the window
        let viewer_title = format!(
            "Image from: {:?}\t\t\t\t\tImage name: {}\t\t\t\t\tRemaining Views: {}",
            src_addr,
            name_without_ext,
            access - 1
        );

        // Create a window to display the image
        let mut window = Window::new(
            viewer_title.as_str(),
            1024,
            768,
            WindowOptions {
                resize: true,
                scale: minifb::Scale::X2,
                ..Default::default()
            },
        )
        .expect("Failed to create window");

        // Main event loop
        while window.is_open() && !window.is_key_down(Key::Escape) {
            // Update the window with the image data
            let buffer: Vec<u32> = decoded_buffer
                .pixels()
                .map(|p| ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32)
                .collect();

            window
                .update_with_buffer(&buffer, width_decoded as usize, height_decoded as usize)
                .expect("Failed to update window");
        }
        true
    }

    pub async fn request_update_access(
        &self,
        img_name: String,
        new_access: Action,
        peer_client_addr: String,
    ) {
        let img_id = format!(
            "{}&{}&{}",
            peer_client_addr,
            self.client_socket.local_addr().unwrap(),
            img_name
        );
        println!("Send Update Access Request");
        let msg = Msg {
            sender: self.client_socket.local_addr().unwrap(),
            receiver: peer_client_addr.parse::<SocketAddr>().unwrap(),
            msg_type: Type::UpdateAccessRequest(img_id, new_access),
            payload: None,
        };
        let serialized_msg = serde_cbor::ser::to_vec(&msg).unwrap();
        self.client_socket
            .send_to(&serialized_msg, peer_client_addr)
            .await
            .unwrap();
    }

    pub async fn handle_update_access_req(
        img_id: String,
        action: Action,
        client_socket: Arc<UdpSocket>,
        src_addr: SocketAddr,
    ) {
        println!("Handle Update Access Request");
        println!("Image ID: {}", img_id);
        println!("Image New Access: {:?}", action);

        let img_id_parts: Vec<&str> = img_id.split('&').collect();
        let img_name = *img_id_parts.last().unwrap();

        let msg = Msg {
            sender: client_socket.local_addr().unwrap(),
            receiver: src_addr,
            msg_type: Type::UpdateAccess(img_id, action),
            payload: None,
        };

        let serialized_msg = serde_cbor::ser::to_vec(&msg).unwrap();
        client_socket
            .send_to(&serialized_msg, src_addr)
            .await
            .unwrap();
    }

    async fn handle_update_access(
        img_id: String,
        action: Action,
        src_addr: SocketAddr,
        received_shared_imgs: Arc<Mutex<HashMap<SocketAddr, Vec<(String, u32)>>>>,
    ) {
        println!("Handle Update Access");
        println!("Image ID: {}", img_id);
        println!("Image New Access: {:?}", action);

        let img_id_parts: Vec<&str> = img_id.split('&').collect();
        let img_name = img_id_parts.last().unwrap();

        let path = format!("{}/{}/{}", ENCRYPTED_PICS_PATH, src_addr, img_name);

        if file_exists(path.as_str()) {
            let mut img_buffer = file_as_image_buffer(path.clone());
            let (width, height) = img_buffer.dimensions();
            let access_pixel = img_buffer.get_pixel_mut(width - 1, height - 1);

            match action {
                Action::Increment(n) => access_pixel[3] += n as u8,
                Action::Decrement(n) => {
                    if n as u8 > access_pixel[3] {
                        access_pixel[3] = 0
                    } else {
                        access_pixel[3] -= n as u8
                    }
                }
                Action::Revoke => access_pixel[3] = 0,
            }
            let updated_access_num = access_pixel[3];

            save_image_buffer(img_buffer, path.clone());

            let mut guard = received_shared_imgs.lock().await;
            let entry = guard.entry(src_addr).or_insert(Vec::new());
            if let Some(index) = entry.iter().position(|(s, _)| s == &img_id) {
                entry[index] = (img_id, updated_access_num as u32);
            } else {
                entry.push((img_id, updated_access_num as u32));
            }
        }
    }

    pub async fn send_update_access_to_client(
        &self,
        img_name: String,
        action: Action,
        peer_client_addr: String,
    ) {
        let socket = self.client_socket.clone();

        let img_id = format!(
            "{}&{}&{}",
            socket.local_addr().unwrap(),
            peer_client_addr,
            img_name
        );

        println!("Sending update access to client {:?}", peer_client_addr);
        let msg = Msg {
            sender: socket.local_addr().unwrap(),
            receiver: peer_client_addr.parse::<SocketAddr>().unwrap(),
            msg_type: Type::UpdateAccess(img_id.clone(), action.clone()),
            payload: None,
        };

        let serialized_msg = serde_cbor::ser::to_vec(&msg).unwrap();
        socket
            .send_to(&serialized_msg, peer_client_addr)
            .await
            .unwrap();
    }

    pub async fn send_update_access_to_cloud(
        &self,
        img_name: String,
        action: Action,
        peer_client_addr: String,
    ) {
        let servers = self.cloud_servers.clone();
        // let mut buffer = [0; 1024];
        let socket = self.cloud_socket.clone();

        let img_id = format!(
            "{}&{}&{}",
            self.client_socket.local_addr().unwrap(),
            peer_client_addr,
            img_name
        );

        for server in &servers {
            let target_addr = server.0;

            println!("Sending pending update access to server {:?}", server);
            let msg = Msg {
                sender: socket.local_addr().unwrap(),
                receiver: target_addr,
                msg_type: Type::UpdateAccess(img_id.clone(), action.clone()),
                payload: None,
            };
            let serialized_msg = serde_cbor::ser::to_vec(&msg).unwrap();
            // let serialized_msg = serde_json::to_string(&msg).unwrap();
            socket.send_to(&serialized_msg, target_addr).await.unwrap();
        }

        // wait for election result (server ip)
        // for _ in 0..5 {
        //     match socket.recv_from(&mut buffer).await {
        //         Ok((_bytes_read, src_addr)) => {
        //             // println!("{}", src_addr);
        //             if let Ok(response) = std::str::from_utf8(&buffer[.._bytes_read]) {
        //                 let chosen_server: SocketAddr = response.parse().unwrap();
        //                 // println!("{}", chosen_server);
        //                 return Some(chosen_server);
        //             } else {
        //                 continue;
        //             }
        //         }
        //         Err(e) => {
        //             eprintln!("Error getting the result of eletion: {}", e);
        //         }
        //     }
        // }
    }

    pub async fn query_pending_updates(&self) -> Option<HashMap<String, Action>> {
        if let Some(chosen_server) = self
            .send_init_request_to_cloud(Type::ClientRequest(1))
            .await
        {
            ClientDirOfService::query_pending(self.cloud_socket.clone(), chosen_server).await;
            let mut buffer = [0; 2048];

            println!("Waiting for pending updates status from cloud");
            let sleep = sleep(Duration::from_millis(500));
            tokio::pin!(sleep);

            for _ in 0..5 {
                tokio::select! {
                    _ = &mut sleep => {
                        continue;
                    },
                    recv_result = self.cloud_socket.recv_from(&mut buffer) => {
                        match recv_result{
                            Ok((bytes_read, src_addr)) => {
                                match serde_cbor::de::from_slice(&buffer[..bytes_read]) {
                                    Ok(Msg {
                                        msg_type: Type::ClientDirOfServQueryPendingReply(r),
                                        ..
                                    }) => return r,
                                    Ok(_) => continue,
                                    Err(e) => continue,
                                }
                            }
                            Err(e) => {
                                eprintln!("Error getting the result of election: {}", e);
                                continue;
                            }
                        }
                    }

                }
            }
        }
        None
    }

    pub async fn query_dir_of_serv(&self) -> Option<HashMap<SocketAddr, bool>> {
        if let Some(chosen_server) = self
            .send_init_request_to_cloud(Type::ClientRequest(1))
            .await
        {
            ClientDirOfService::query(self.cloud_socket.clone(), chosen_server).await;

            let mut buffer = [0; 1024];
            for _ in 0..5 {
                match self.cloud_socket.recv_from(&mut buffer).await {
                    Ok((bytes_read, src_addr)) => {
                        match serde_cbor::de::from_slice(&buffer[..bytes_read]) {
                            Ok(Msg {
                                msg_type: Type::DirOfServQueryReply(r),
                                ..
                            }) => return Some(r),
                            Ok(_) => continue,
                            Err(e) => continue,
                        }
                    }
                    Err(e) => {
                        eprintln!("Error getting the result of election: {}", e);
                        continue;
                    }
                }
            }
        }
        None
    }

    pub async fn encrypt(&mut self, pic_file_path: &str) {
        create_output_dirs();
        let pic_paths = get_pic_paths(pic_file_path);
        for pic_path in &pic_paths {
            let pic_path = Path::new(pic_path);
            let pic_with_ext = pic_path.file_name().unwrap().to_str().unwrap();
            let pic_without_ext = pic_path.file_stem().unwrap().to_str().unwrap();
            let pic_path = format!("{}/{}", HIGH_RES_PICS_PATH, pic_with_ext);
            // let pic_path = pic_path.to_str().unwrap();
            let socket = self.cloud_socket.clone();
            let id = self.get_next_req_id();

            // trigger election
            if let Some(chosen_server) = self
                .send_init_request_to_cloud(Type::ClientRequest(id))
                .await
            {
                let chosen_server = chosen_server.to_string();
                println!(
                    "Sending image ({}) to server {} for ENCRYPTION",
                    pic_with_ext, chosen_server
                );

                // send the img to be encrypted
                let contents = fs::read(pic_path).await.unwrap();
                let msg_id = format!("{}:{}", socket.local_addr().unwrap(), id);
                // println!("Message Length {}", contents.len());
                fragment::client_send(contents, socket.clone(), &chosen_server, &msg_id, false)
                    .await;
                println!("Finished sending pic");

                let encoded_bytes = fragment::receive_all(socket.clone()).await;
                let encoded_image: Image = serde_cbor::de::from_slice(&encoded_bytes).unwrap();

                let image_buffer: image::ImageBuffer<Rgba<u8>, Vec<u8>> =
                    image::ImageBuffer::from_raw(
                        encoded_image.dims.0,
                        encoded_image.dims.1,
                        encoded_image.data,
                    )
                    .unwrap();

                save_image_buffer(
                    image_buffer.clone(),
                    format!("{}/{}.png", ENCRYPTED_PICS_PATH, pic_without_ext),
                );

                // let secret_bytes = decode_img(image_buffer).await;
                // let _ = tokio::fs::write(format!("secret_{pic_with_ext}"), secret_bytes).await;
            } else {
                println!("Failed to get the election result (server ip)");
            };
        }
    }

    pub async fn quit(&self) {
        ClientDirOfService::leave(self.cloud_socket.clone(), self.cloud_servers.clone()).await;
        std_fs::write(REQ_ID_LOG_FILEPATH, self.next_req_id.to_string())
            .expect("Failed to write req_id to req_id_log.txt");
        // complete logic for quit
    }

    fn get_next_req_id(&mut self) -> u32 {
        let id = self.next_req_id;
        self.next_req_id += 1;
        id
    }
}

fn save_image_buffer(image_buffer: image::ImageBuffer<Rgba<u8>, Vec<u8>>, filename: String) {
    let image = image::DynamicImage::ImageRgba8(image_buffer);
    image.save(filename).unwrap();
}

fn file_as_image_buffer(filename: String) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let img = open(filename).unwrap();
    img.to_rgba8()
}
