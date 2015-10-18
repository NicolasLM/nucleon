Nucleon
=======

Nucleon is a dynamic TCP load balancer written in Rust. It has the ability to
insert and remove backend servers on the flight. To do that it leverages [Redis
Pub/Sub](http://redis.io/topics/pubsub) mechanism. Adding or removing a server
to a cluster is as easy as publishing a message to Redis.

How to build it
---------------

All you need to build it is [Rust
1.3](https://doc.rust-lang.org/stable/book/installing-rust.html).

Just go in the repository and issue:

    $ cargo build --release

Usage
-----

Nucleon can be used with or without a Redis database. When ran without Redis it
is not possible to add or remove load balanced servers without restarting the
process.

```
Usage:
    nucleon [OPTIONS] [SERVER ...]

Dynamic TCP load balancer

positional arguments:
  server                Servers to load balance

optional arguments:
  -h,--help             show this help message and exit
  -b,--bind BIND        Bind the load balancer to address:port (127.0.0.1:8000)
  -r,--redis REDIS      URL of Redis database (redis://localhost)
  --no-redis            Disable updates of backend through Redis
  -l,--log LOG          Log level [debug, info, warn, error] (info)
```

Imagine you have two web servers to load balance, and a local Redis. Run the
load balancer with:

    nucleon --bind 0.0.0.0:8000 10.0.0.1:80 10.0.0.2:80 

Now imagine that you want to scale up your infrastructure by spawning a new web
server at 10.0.0.3. Just send a message to the Redis channel `backend_add`:

    redis:6379> PUBLISH backend_add 10.0.0.3:80
    (integer) 1

Your will see in logs:

    INFO - Load balancing server V4(10.0.0.1:80)
    INFO - Load balancing server V4(10.0.0.2:80)
    INFO - Now listening on 0.0.0.0:8000
    INFO - Subscribed to Redis channels 'backend_add' and 'backend_remove'
    INFO - Added new server 10.0.0.3:80

If you decide that you do not need server 2 any longer:

    redis:6379> PUBLISH backend_remove 10.0.0.2:80
    (integer) 1

How does it perform?
--------------------

Surprisingly well. A quick comparison with HA Proxy in TCP mode with a single
backend containing a single server using iperf results in:

| Connections |   HA Proxy   |    Nucleon    |
| -----------:| ------------:| -------------:|
|           1 | 15.1 Gbits/s | 15.7 Gbits/s  |
|          10 | 13.5 Gbits/s | 11.3 Gbits/s  |
|         100 |  8.9 Gbits/s | 10.5 Gbits/s  |

Keep in mind that this is a really simple test, far from what real life traffic
looks like. A real benchmark should compare short lived connections with long
running one, etc.

Licence
-------

MIT
