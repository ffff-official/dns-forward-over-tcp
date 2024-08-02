# DNS Forward over TCP

As a dns server. forward data over TCP.

# How to use?

#### run server

```shell
./dns-forward-over-tcp -p 5353 -u 8.8.8.8:53
```

#### test 
```shell
dig @127.0.0.1 -p 5353 google.com
```
