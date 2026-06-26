# LambdaDoom project tasks.

LAUNCH_BUCKET ?= lambdadoom-launch-932930471665
LAUNCH_REGION ?= us-east-1

.PHONY: sync-template

# Publish CloudFormation template for the Launch Stack button.
sync-template:
	aws s3 cp deploy/doom.yaml s3://$(LAUNCH_BUCKET)/doom.yaml --region $(LAUNCH_REGION) --content-type text/yaml
	@echo "synced deploy/doom.yaml -> s3://$(LAUNCH_BUCKET)/doom.yaml"
