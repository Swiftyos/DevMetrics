# Git LoC Tracker

## Overview

The Git LoC Tracker is a Rust-based application designed to monitor and track lines of code (LoC) changes in specified Git repositories. It provides insights into the contributions made by a specific author, including both committed and pending changes.

## Features

- **Track Changes**: Monitors specified Git repositories for changes made by a particular author.
- **Database Storage**: Stores line of code changes in a SQLite database for persistent tracking.
- **Real-time Updates**: Uses a file watcher to detect changes in the repositories and updates the database accordingly.
- **Statistics Reporting**: Provides a summary of committed and pending lines of code for each repository being tracked.

## Getting Started

### Prerequisites

- Rust programming language installed on your machine.
- SQLite database for storing the changes.

### Installation

1. Clone the repository:
   ```bash
   git clone <repository-url>
   cd <repository-directory>
   ```

2. Build the project:
   ```bash
   cargo build
   ```

### Usage

To run the application, use the following command:
