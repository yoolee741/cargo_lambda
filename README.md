# cargo-lambda
* Deploying a Lambda function with Cargo Lambda and connecting to AWS Event Bridge

# Step by Step
### 1. Install Cargo Lambda
```
brew tap cargo-lambda/cargo-lambda
brew install cargo-lambda
```

### 2. Create a lambda function via AWS console

![스크린샷 2024-10-25 오전 8 49 54](https://github.com/user-attachments/assets/3e54d5b5-a606-4211-8c10-d14ac7d37664)


### 3. Build (compile your function for Linux ARM architectures)
```
cargo lambda build --release --arm64
```

### 4. Deploy Rust Lambda functions with .zip file archives
```

# In my case, I just transferred files from the environment_lambda repository, so the folder name inside the Lambda foler is environment_lambda.

cd target/lambda/<Your_Repository_Name_Folder>

# Zipping the bootstrap file, which is created after the build.

zip bootstrap.zip bootstrap 

```

### 5. Check if the zip file can operate correctly before upload.
```

# Go to the lambda folder where the zip file is located
cd target/lambda/<Your_Repository_Name_Folder>

# Unzip the file to check the structure within the zip file
unzip -l bootstrap.zip

# Check whether the bootstrap file can operate correctly
file bootstrap

# Expected output after running the command
bootstrap: ELF 64-bit LSB pie executable, ARM aarch64, version 1 (SYSV), dynamically linked, interpreter /lib/ld-linux-aarch64.so.1, for GNU/Linux 2.0.0, stripped

# Grant execute permissions to the bootstrap file
chmod +x bootstrap

# Re-zip the unzipped file
zip -j bootstrap.zip bootstrap

```

### 6. Upload the zip file to the Lambda function

#### 1. With Command
```
aws lambda update-function-code --function-name <Your_Lambda_Function_Name> --zip-file fileb://bootstrap.zip
```

#### 2. Via AWS Console
* AWS Console > Lambda > Functions > <Your Lambda Function>

![스크린샷 2024-10-25 오전 9 16 00](https://github.com/user-attachments/assets/9b27d7d7-5845-4954-9214-7464a8babf46)

#### 3. If you have environment variables, make sure to add them

![스크린샷 2024-10-25 오전 9 53 52](https://github.com/user-attachments/assets/3fae206f-6819-42f8-8e71-0c6aa3111db8)



### 7. Connect to the AWS Event Bridge
* Click "Add Trigger"
  
![스크린샷 2024-10-25 오전 9 46 21](https://github.com/user-attachments/assets/f31bde07-cc4f-4b09-a022-78329a410111)

* Select EventBridge (CloudWatch Events)
  
![스크린샷 2024-10-25 오전 9 48 21](https://github.com/user-attachments/assets/27e54296-a5bb-42cb-be9e-c6f810a95f9f)

* Create a new rule for scheduling or choose an existing rule
  

# References
* Cargo Lambda: https://www.cargo-lambda.info/guide/getting-started.html & https://www.cargo-lambda.info/commands/build.html
* AWS SDK for Rust: https://docs.aws.amazon.com/sdk-for-rust/latest/dg/lambda.html
* AWS Lambda: https://docs.aws.amazon.com/lambda/latest/dg/rust-package.html
* When to use Lambda's OS-only runtimes: https://docs.aws.amazon.com/lambda/latest/dg/runtimes-provided.html
* Creating a rule that runs on a schedule in Amazon EventBridge (AWS EventBridge): https://docs.aws.amazon.com/eventbridge/latest/userguide/eb-create-rule-schedule.html
