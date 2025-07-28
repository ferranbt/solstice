// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.0;

contract Parent {
    uint256 public val;

    function setValue(uint256 _value) public {
        val = _value;
    }
}

contract Complete {
    function setOtherValue(uint256 _value) public {
        Parent p = new Parent();
        p.
    }
}
