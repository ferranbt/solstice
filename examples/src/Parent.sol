pragma solidity ^0.8.0;

contract Parent {
    uint256 value3;

    constructor() {
        value3 = 0;
    }
    
    function setOtherValue(uint256 _value) public {
        value3 += _value;
    }
}
