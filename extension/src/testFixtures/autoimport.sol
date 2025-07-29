// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.0;

contract AutoImport {
    function go() public {
        Parent p = new Parent();
    }

    function go2() public {
        Definitions d = new Definitions();
    }
}
