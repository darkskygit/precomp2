# Vendored liblz4 sources

Test oracle only; never linked into precomp2. Each directory is an unmodified
subset of the upstream release tag `https://github.com/lz4/lz4/archive/refs/tags/<tag>.tar.gz` (`r131`, `v1.9.1`, `v1.10.0`):
`lib/` files as-is, and `LICENSE` is upstream `lib/LICENSE` (BSD-2-Clause).
`tests/lz4_reference.rs` checks `LZ4_VERSION_NUMBER` against each release (r131 declares 1.7.1).

```
8b58c446121a109ccf32edc094bba3010a3d85e4ee3702950db55e4d3e87736c  1.10.0/LICENSE
9396f7de527bc8435de9c7569fb7998e56545a84b4f3c2d808c0235c01774539  1.10.0/lib/lz4.c
26b82efc53d1570f3b54eef02e9c4764c1ad374ff03cac04e2ced5ea4d4c552f  1.10.0/lib/lz4.h
126cafafdb91767e6e55238298a910903851b35b2cee27ce80ae2280469ee232  1.10.0/lib/lz4hc.c
e43824e8a9ba16f54100c4ccbccfa5782a858ca9ab83c48aac303fea3e76e21e  1.10.0/lib/lz4hc.h
d15d99c8dc6b0ec22174c0e563a95bc40f9363ca7f9d9d793bb5c5a8e8d0af71  1.9.1/LICENSE
0d224705939383a1ccd90d0d2ee3bfffff696ec2c596916d6a8e5a5cc7d150cc  1.9.1/lib/lz4.c
55f9541ed5d26a6d906cc430ccfdb3caddea757bbc57ff92b2f658545d0cf14a  1.9.1/lib/lz4.h
36281a17708552b194ac70879400a352d660cd59d0b93faa70e86cdf55f522dd  1.9.1/lib/lz4hc.c
74d1675e5a51eb33ac6b94258b737a85e94f990dc8e2ba634d738c8f7e3b075d  1.9.1/lib/lz4hc.h
cd680953c2d0933069840846381b904ef2c02e4cfe6e40e89c4417876f10b2d2  r131/LICENSE
08213978424040ecdd0ce3bc8668cab3fd8f12cfddbda45bd9e372a10c16e495  r131/lib/lz4.c
ee1cc5ab90016e75d62cfc21c42f029f7f495de5cc37b72eca20414b7e32562e  r131/lib/lz4.h
f47d006f914a5a90cf28dc1c6beb7fbecd58cea128dd21a5901a47df9865128a  r131/lib/lz4hc.c
06482c03dd67b87abb610edbb238b0b413551eb15522c399c339808b9bd0159e  r131/lib/lz4hc.h
```
