import {Test} from "forge-std/Test.sol";
import {Parent} from "../src/Parent.sol";

contract FooTest is Test {
    struct Example {
        uint256 value_storage;
    }
    
    uint256 value_storage;
    Example[2][] second_storage;

    function test_Example() external {
        uint256 value = block.number; // Non-constant value
        value = value + 1;
        if (value < 10) {
            value = value + 1;
        }
        Example memory example = Example({
            value_storage: value
        });
        value_storage = 1;
        value_storage = 2;
        value_storage = 3;
        simple_call();
        value = value + simple_call();
        value = value + call_with_params(11, 22);
        assert(value != block.number);
    }

    function simple_call() private returns (uint256) {
        return block.number + 2;
    }

    function call_with_params(uint256 a, uint256 b) private returns (uint256) {
        return a + b;
    }
}
