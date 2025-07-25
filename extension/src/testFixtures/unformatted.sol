// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract   TokenVault{
mapping(address=>uint256)private balances;
mapping(address=>bool)public admins;
uint256 public totalDeposits;
address public owner;

event   Deposit(address indexed user,uint256 amount);
event Withdrawal(address indexed user,uint256 amount);
event AdminAdded(address indexed admin);

modifier onlyOwner(){require(msg.sender==owner,"Not owner");_;}
modifier onlyAdmin(){require(admins[msg.sender]||msg.sender==owner,"Not admin");_;}

constructor(){
owner=msg.sender;admins[msg.sender]=true;
}

function deposit()external payable{
require(msg.value>0,"Amount must be greater than 0");
balances[msg.sender]+=msg.value;totalDeposits+=msg.value;
emit Deposit(msg.sender,msg.value);
}

function withdraw(uint256 amount)external{
require(amount>0,"Invalid amount");
require(balances[msg.sender]>=amount,"Insufficient balance");
balances[msg.sender]-=amount;totalDeposits-=amount;
payable(msg.sender).transfer(amount);emit Withdrawal(msg.sender,amount);
}

function getBalance(address user)external view returns(uint256){
return balances[user];
}

function addAdmin(address newAdmin)external onlyOwner{
require(newAdmin!=address(0),"Invalid address");
require(!admins[newAdmin],"Already admin");
admins[newAdmin]=true;emit AdminAdded(newAdmin);
}

function emergencyWithdraw(address user,uint256 amount)external onlyAdmin{
require(balances[user]>=amount,"Insufficient user balance");
balances[user]-=amount;totalDeposits-=amount;
payable(user).transfer(amount);
}

struct   LockInfo{uint256 amount;uint256 unlockTime;}
mapping(address=>LockInfo)public lockedFunds;

function lockFunds(uint256 amount,uint256 lockDuration)external{
require(amount>0&&lockDuration>0,"Invalid parameters");
require(balances[msg.sender]>=amount,"Insufficient balance");
balances[msg.sender]-=amount;
lockedFunds[msg.sender]=LockInfo({amount:lockedFunds[msg.sender].amount+amount,unlockTime:block.timestamp+lockDuration});
}

function unlockFunds()external{
LockInfo memory lockInfo=lockedFunds[msg.sender];
require(lockInfo.amount>0,"No locked funds");
require(block.timestamp>=lockInfo.unlockTime,"Funds still locked");
balances[msg.sender]+=lockInfo.amount;
delete lockedFunds[msg.sender];
}

function batchTransfer(address[]memory recipients,uint256[]memory amounts)external onlyAdmin{
require(recipients.length==amounts.length,"Arrays length mismatch");
for(uint256 i=0;i<recipients.length;i++){
require(recipients[i]!=address(0),"Invalid recipient");
require(amounts[i]>0,"Invalid amount");
balances[recipients[i]]+=amounts[i];
totalDeposits+=amounts[i];
}
}

function calculateFee(uint256 amount)public pure returns(uint256){
if(amount<1 ether)return amount*1/100;
else if(amount<10 ether)return amount*2/100;
else return amount*3/100;
}

receive()external payable{
balances[msg.sender]+=msg.value;totalDeposits+=msg.value;
emit Deposit(msg.sender,msg.value);
}

fallback()external payable{
revert("Function not found");
}
}
