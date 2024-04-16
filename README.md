# A Cloud P2P Environment for Controlled Sharing of Images

This is a university project for the Fundamentals of Distributed Systems Course written in rust. The goal of the project is to have a service of encryption on the cloud. Clients of the service send images to be encrypted. The images are encrypted and then sent back to them. No copies of the images are kept by the service provider.


The service provider (the cloud) in our case is a cluster of 3 servers. The servers, among themselves, are communicating in a peer-to-peer fashion. They are on equal footing; and they use an election algorithm to distribute the workload, i.e. load balancing. They also use the election algorithm to simulate failures. In other words, at random times, a server is elected to go down. The service should be uninterrupted in the case of such failure, and the server should be updated with the state of the system whenever it recovers from failure.


Another goal is to provide a directory of services that shows active clients. Using the directory of services, clients can connect together and share images in a peer to peer communication paradigm. The owner of the picture can constraint the shared image with a certain number of views. This number can be adjusted after the image is shared. A mechanism will be implemented to allow the enforcement of these adjustments once the client with which the images are shared is online.


To read more about the requirements of the project, the design choices, testing results and the user interface please refer to [this document](./Distributed%20Project.pdf).
