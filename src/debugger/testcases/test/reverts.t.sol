// SPDX-License-Identifier: UNLICENSED
import {Test} from "forge-std/Test.sol";

contract Reverts is Test {
    function test_simple_revert() public {
        uint256 value = 0;
        revert("This is a revert message");
    }

    function test_nested_revert() public {
        uint256 value2 = 0;
        test_simple_revert();
    }
}
