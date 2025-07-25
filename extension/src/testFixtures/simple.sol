pragma solidity ^0.8.0;

contract Parent {
    uint256 public value3;

    function setOtherValue(uint256 _value) public {
        value3 += _value;
    }
}
