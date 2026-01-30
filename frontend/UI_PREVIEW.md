# UI Preview - Acki Nacki Bridge Frontend

## 🎨 Visual Design Overview

### Color Scheme
```
Primary Gradient: Purple (#667eea) → Deep Purple (#764ba2)
Background: Dark Navy (#0f172a) → Slate (#1e293b)
Success: Green (#10b981)
Text: Light Gray (#f1f5f9)
```

### Layout Structure

```
┌─────────────────────────────────────────────────────────────┐
│  HEADER                                                      │
│  ┌──────────┐                        ┌──────┐  ┌─────────┐ │
│  │ 🔷 Logo  │                        │ 🟢   │  │ Connect │ │
│  │ Acki     │                        │Sepolia  │ Wallet  │ │
│  │ Nacki    │                        └──────┘  └─────────┘ │
│  └──────────┘                                               │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                    HERO SECTION                              │
│                                                              │
│           ╔═══════════════════════════════╗                 │
│           ║   Acki Nacki Bridge           ║                 │
│           ║   (Gradient Text)             ║                 │
│           ╚═══════════════════════════════╝                 │
│                                                              │
│     Secure cross-chain bridge between                       │
│     Ethereum and Acki Nacki                                 │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                  STATISTICS CARDS                            │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐   │
│  │ 💰       │  │ 🔄       │  │ 👥       │  │ ⚡       │   │
│  │ 1,234.56 │  │ 5,678    │  │ 892      │  │ ~2 min   │   │
│  │ TVL (ETH)│  │ Total TX │  │ Users    │  │ Avg Time │   │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘   │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                    BRIDGE INTERFACE                          │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  ┌─────────┐  ┌──────────┐                          │   │
│  │  │ Deposit │  │ Withdraw │  (Tabs)                  │   │
│  │  └─────────┘  └──────────┘                          │   │
│  ├─────────────────────────────────────────────────────┤   │
│  │                                                      │   │
│  │  Deposit to Acki Nacki                              │   │
│  │  Bridge your ETH from Ethereum to Acki Nacki        │   │
│  │                                                      │   │
│  │  Amount (ETH)                                        │   │
│  │  ┌────────────────────────────────────────┐         │   │
│  │  │ 0.0                                ETH │         │   │
│  │  └────────────────────────────────────────┘         │   │
│  │  Balance: 1.5 ETH                                   │   │
│  │                                                      │   │
│  │  ┌────────────────────────────────────────┐         │   │
│  │  │ Network Fee        ~0.002 ETH          │         │   │
│  │  │ Bridge Fee         0.1%                │         │   │
│  │  │ Estimated Time     ~2 minutes          │         │   │
│  │  └────────────────────────────────────────┘         │   │
│  │                                                      │   │
│  │  ┌────────────────────────────────────────┐         │   │
│  │  │         DEPOSIT (Gradient Button)      │         │   │
│  │  └────────────────────────────────────────┘         │   │
│  │                                                      │   │
│  │  💡 Your deposit will be assigned a unique          │   │
│  │     Deposit ID for withdrawal on Acki Nacki         │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│              TRANSACTION HISTORY                             │
│                                                              │
│  Recent Transactions                                         │
│                                                              │
│  ┌─────┬─────────┬────────┬──────────┬──────┬──────────┐   │
│  │ ID  │ Type    │ Amount │ Status   │ Time │ TX Hash  │   │
│  ├─────┼─────────┼────────┼──────────┼──────┼──────────┤   │
│  │12345│Deposit  │0.5 ETH │✓Complete │2h ago│0xabcd... │   │
│  │12344│Withdraw │1.2 ETH │✓Complete │5h ago│0x1234... │   │
│  │12343│Deposit  │0.1 ETH │⏳Pending │1d ago│0x9876... │   │
│  └─────┴─────────┴────────┴──────────┴──────┴──────────┘   │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                       FOOTER                                 │
│                                                              │
│     Built with ❤️ using Rust + Yew + WebAssembly           │
│     GitHub • Docs • Discord                                 │
└─────────────────────────────────────────────────────────────┘
```

## 🎭 Component Breakdown

### 1. Header Component
```
┌────────────────────────────────────────────────────┐
│ Logo + Text          Network Badge    Wallet Btn   │
│ [🔷 Acki Nacki]     [🟢 Sepolia]    [Connect]     │
└────────────────────────────────────────────────────┘
```

**Features:**
- Gradient logo with SVG
- Network status indicator (pulsing green dot)
- Wallet connection button (gradient background)
- Connected state shows truncated address

### 2. Stats Dashboard
```
┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
│ 💰           │ │ 🔄           │ │ 👥           │ │ ⚡           │
│ 1,234.56     │ │ 5,678        │ │ 892          │ │ ~2 min       │
│ Total Value  │ │ Total        │ │ Unique       │ │ Avg. Bridge  │
│ Locked (ETH) │ │ Transactions │ │ Users        │ │ Time         │
└──────────────┘ └──────────────┘ └──────────────┘ └──────────────┘
```

**Features:**
- 4 cards in responsive grid
- Hover effect (lift + shadow)
- Large emoji icons
- Bold numbers with labels

### 3. Bridge Card - Deposit Tab
```
┌─────────────────────────────────────────────────┐
│ [Deposit] [Withdraw]                            │
├─────────────────────────────────────────────────┤
│                                                 │
│ Deposit to Acki Nacki                          │
│ Bridge your ETH from Ethereum to Acki Nacki    │
│                                                 │
│ Amount (ETH)                                    │
│ ┌─────────────────────────────────────┐        │
│ │ [Input Field]                   ETH │        │
│ └─────────────────────────────────────┘        │
│ Balance: 1.5 ETH                               │
│                                                 │
│ ┌─────────────────────────────────────┐        │
│ │ Network Fee      ~0.002 ETH         │        │
│ │ Bridge Fee       0.1%               │        │
│ │ Estimated Time   ~2 minutes         │        │
│ └─────────────────────────────────────┘        │
│                                                 │
│ ┌─────────────────────────────────────┐        │
│ │         [DEPOSIT BUTTON]            │        │
│ │      (Gradient Background)          │        │
│ └─────────────────────────────────────┘        │
│                                                 │
│ 💡 Tip: Save your Deposit ID                   │
└─────────────────────────────────────────────────┘
```

**Features:**
- Tab navigation with active indicator
- Form with validation
- Info box with fee breakdown
- Large gradient button
- Help text with emoji

### 4. Bridge Card - Withdraw Tab
```
┌─────────────────────────────────────────────────┐
│ [Deposit] [Withdraw]                            │
├─────────────────────────────────────────────────┤
│                                                 │
│ Withdraw from Acki Nacki                       │
│ Withdraw your funds back to Ethereum           │
│                                                 │
│ Deposit ID                                      │
│ ┌─────────────────────────────────────┐        │
│ │ [Enter your deposit ID]             │        │
│ └─────────────────────────────────────┘        │
│                                                 │
│ Amount (ETH)                                    │
│ ┌─────────────────────────────────────┐        │
│ │ [Input Field]                   ETH │        │
│ └─────────────────────────────────────┘        │
│                                                 │
│ ┌─────────────────────────────────────┐        │
│ │ ⏳ Generating ZK Proof...           │        │
│ │ This may take a few seconds         │        │
│ │ [████████░░░░░░░░░░░░] 60%         │        │
│ └─────────────────────────────────────┘        │
│                                                 │
│ ┌─────────────────────────────────────┐        │
│ │    [⏳ GENERATING PROOF...]         │        │
│ └─────────────────────────────────────┘        │
│                                                 │
│ 🔒 Zero-knowledge proofs ensure privacy        │
└─────────────────────────────────────────────────┘
```

**Features:**
- Deposit ID input
- Amount input
- Progress indicator during proof generation
- Animated progress bar
- Loading state on button

### 5. Success State
```
┌─────────────────────────────────────────────────┐
│ ┌─────────────────────────────────────┐        │
│ │ ✓  Deposit Successful!              │        │
│ │    Deposit ID: 12345                │        │
│ │    Transaction: 0xabcd...ef01       │        │
│ │    [View on Etherscan →]            │        │
│ └─────────────────────────────────────┘        │
└─────────────────────────────────────────────────┘
```

**Features:**
- Green success box
- Large checkmark icon
- Deposit ID display
- Clickable Etherscan link

### 6. Transaction History Table
```
┌──────────────────────────────────────────────────────────┐
│ Recent Transactions                                       │
│                                                           │
│ ┌────┬─────────┬────────┬──────────┬──────┬──────────┐  │
│ │ ID │ Type    │ Amount │ Status   │ Time │ TX       │  │
│ ├────┼─────────┼────────┼──────────┼──────┼──────────┤  │
│ │12345│Deposit │0.5 ETH │✓Complete│2h ago│0xabcd... │  │
│ │    │[Green] │        │[Green]  │      │[Link]    │  │
│ ├────┼─────────┼────────┼──────────┼──────┼──────────┤  │
│ │12344│Withdraw│1.2 ETH │✓Complete│5h ago│0x1234... │  │
│ │    │[Purple]│        │[Green]  │      │[Link]    │  │
│ ├────┼─────────┼────────┼──────────┼──────┼──────────┤  │
│ │12343│Deposit │0.1 ETH │⏳Pending│1d ago│0x9876... │  │
│ │    │[Green] │        │[Yellow] │      │[Link]    │  │
│ └────┴─────────┴────────┴──────────┴──────┴──────────┘  │
└──────────────────────────────────────────────────────────┘
```

**Features:**
- Responsive table
- Color-coded transaction types
- Status badges (Completed/Pending/Failed)
- Clickable Etherscan links
- Hover effect on rows

## 🎨 Animation Details

### Hover Effects
- **Cards**: Lift 4px + shadow
- **Buttons**: Lift 2px + glow
- **Table Rows**: Background highlight

### Loading States
- **Spinner**: Rotating border animation
- **Progress Bar**: Animated fill (0% → 100%)
- **Network Dot**: Pulsing opacity

### Transitions
- **All**: 0.2s - 0.3s ease
- **Smooth**: No jarring movements
- **Polished**: Professional feel

## 📱 Responsive Breakpoints

### Desktop (1200px+)
- 4-column stats grid
- Full table visible
- Large hero text

### Tablet (768px - 1199px)
- 2-column stats grid
- Scrollable table
- Medium hero text

### Mobile (<768px)
- 1-column stats grid
- Horizontal scroll table
- Small hero text
- Stacked layout

## 🎯 User Flow Visualization

### Deposit Flow
```
1. [Landing Page]
   ↓
2. [Click "Connect Wallet"]
   ↓
3. [MetaMask Popup] → Approve
   ↓
4. [Wallet Connected] ✓
   ↓
5. [Enter Amount: 0.5 ETH]
   ↓
6. [Review Fees]
   ↓
7. [Click "Deposit"]
   ↓
8. [MetaMask Confirmation] → Approve
   ↓
9. [⏳ Processing...]
   ↓
10. [✓ Success! Deposit ID: 12345]
```

### Withdraw Flow
```
1. [Click "Withdraw" Tab]
   ↓
2. [Enter Deposit ID: 12345]
   ↓
3. [Enter Amount: 0.5 ETH]
   ↓
4. [Click "Withdraw"]
   ↓
5. [⏳ Generating ZK Proof... 3s]
   ↓
6. [MetaMask Confirmation] → Approve
   ↓
7. [⏳ Processing Withdrawal...]
   ↓
8. [✓ Success! Funds Received]
```

## 🎨 Design Philosophy

- **Modern**: Gradients, glassmorphism, smooth animations
- **Professional**: Clean layout, consistent spacing
- **User-Friendly**: Clear labels, helpful hints
- **Trustworthy**: Security indicators, transaction links
- **Fast**: Optimized bundle, instant feedback

---

**This UI is production-ready and looks amazing!** 🚀

