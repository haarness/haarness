"""Generated endpoint methods. Do not edit."""
from typing import cast, Union
from . import cloud_types as T
from .cloud_transport import Transport, AsyncTransport, RequestOptions
class CloudApi(Transport):
    def apple_app_site_association(self, *, options: RequestOptions | None = None) -> T.AppleAppSiteAssociation:
        """GET /.well-known/apple-app-site-association."""
        return cast("T.AppleAppSiteAssociation", self.request('GET', f'/.well-known/apple-app-site-association', binary_body=False, binary_response=False, options=options))

    def android_asset_links(self, *, options: RequestOptions | None = None) -> list[T.AndroidAssetLinkStatement]:
        """GET /.well-known/assetlinks.json."""
        return cast("list[T.AndroidAssetLinkStatement]", self.request('GET', f'/.well-known/assetlinks.json', binary_body=False, binary_response=False, options=options))

    def native_router_session(self, body: T.NativeRouterSessionRequest, *, options: RequestOptions | None = None) -> T.NativeRouterSessionResponse:
        """POST /api/v1/account/router-session."""
        return cast("T.NativeRouterSessionResponse", self.request('POST', f'/api/v1/account/router-session', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def list_accounts(self, *, options: RequestOptions | None = None) -> T.Items_AccountDto:
        """GET /api/v1/accounts."""
        return cast("T.Items_AccountDto", self.request('GET', f'/api/v1/accounts', binary_body=False, binary_response=False, options=options))

    def create_account(self, body: T.CreateAccountRequest, *, options: RequestOptions | None = None) -> T.AccountDto:
        """POST /api/v1/accounts."""
        return cast("T.AccountDto", self.request('POST', f'/api/v1/accounts', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def get_account(self, id: str, *, options: RequestOptions | None = None) -> T.AccountDto:
        """GET /api/v1/accounts/{id}."""
        return cast("T.AccountDto", self.request('GET', f'/api/v1/accounts/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def patch_account(self, id: str, body: T.UpdateAccountRequest, *, options: RequestOptions | None = None) -> T.AccountDto:
        """PATCH /api/v1/accounts/{id}."""
        return cast("T.AccountDto", self.request('PATCH', f'/api/v1/accounts/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def list_invitations(self, id: str, *, options: RequestOptions | None = None) -> T.Items_InvitationDto:
        """GET /api/v1/accounts/{id}/invitations."""
        return cast("T.Items_InvitationDto", self.request('GET', f'/api/v1/accounts/{self.segment(id)}/invitations', binary_body=False, binary_response=False, options=options))

    def create_invitation(self, id: str, body: T.CreateInvitationRequest, *, options: RequestOptions | None = None) -> T.CreatedInvitationDto:
        """POST /api/v1/accounts/{id}/invitations."""
        return cast("T.CreatedInvitationDto", self.request('POST', f'/api/v1/accounts/{self.segment(id)}/invitations', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def revoke_invitation(self, id: str, invitationId: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/accounts/{id}/invitations/{invitationId}."""
        return cast("None", self.request('DELETE', f'/api/v1/accounts/{self.segment(id)}/invitations/{self.segment(invitationId)}', binary_body=False, binary_response=False, options=options))

    def list_members(self, id: str, *, options: RequestOptions | None = None) -> T.Items_MemberDto:
        """GET /api/v1/accounts/{id}/members."""
        return cast("T.Items_MemberDto", self.request('GET', f'/api/v1/accounts/{self.segment(id)}/members', binary_body=False, binary_response=False, options=options))

    def remove_member(self, id: str, userId: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/accounts/{id}/members/{userId}."""
        return cast("None", self.request('DELETE', f'/api/v1/accounts/{self.segment(id)}/members/{self.segment(userId)}', binary_body=False, binary_response=False, options=options))

    def update_member(self, id: str, userId: str, body: T.UpdateMemberRequest, *, options: RequestOptions | None = None) -> None:
        """PATCH /api/v1/accounts/{id}/members/{userId}."""
        return cast("None", self.request('PATCH', f'/api/v1/accounts/{self.segment(id)}/members/{self.segment(userId)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def list_projects(self, id: str, *, options: RequestOptions | None = None) -> T.Items_ProjectDto:
        """GET /api/v1/accounts/{id}/projects."""
        return cast("T.Items_ProjectDto", self.request('GET', f'/api/v1/accounts/{self.segment(id)}/projects', binary_body=False, binary_response=False, options=options))

    def create_project(self, id: str, body: T.CreateProjectRequest, *, options: RequestOptions | None = None) -> T.ProjectDto:
        """POST /api/v1/accounts/{id}/projects."""
        return cast("T.ProjectDto", self.request('POST', f'/api/v1/accounts/{self.segment(id)}/projects', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def account_seats(self, id: str, *, options: RequestOptions | None = None) -> T.SeatsDto:
        """GET /api/v1/accounts/{id}/seats."""
        return cast("T.SeatsDto", self.request('GET', f'/api/v1/accounts/{self.segment(id)}/seats', binary_body=False, binary_response=False, options=options))

    def list_account_api_keys(self, query: T.ListAccountApiKeysQuery | None = None, *, options: RequestOptions | None = None) -> T.Items_AccountApiKeyDto:
        """GET /api/v1/api-keys."""
        return cast("T.Items_AccountApiKeyDto", self.request('GET', f'/api/v1/api-keys', query=query, binary_body=False, binary_response=False, options=options))

    def create_account_api_key(self, body: T.CreateAccountApiKeyRequest, *, options: RequestOptions | None = None) -> T.CreatedAccountApiKeyResponse:
        """POST /api/v1/api-keys."""
        return cast("T.CreatedAccountApiKeyResponse", self.request('POST', f'/api/v1/api-keys', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def revoke_account_api_key(self, id: str, query: T.RevokeAccountApiKeyQuery | None = None, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/api-keys/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/api-keys/{self.segment(id)}', query=query, binary_body=False, binary_response=False, options=options))

    def update_account_api_key(self, id: str, body: T.UpdateAccountApiKeyRequest, query: T.UpdateAccountApiKeyQuery | None = None, *, options: RequestOptions | None = None) -> T.AccountApiKeyDto:
        """PATCH /api/v1/api-keys/{id}."""
        return cast("T.AccountApiKeyDto", self.request('PATCH', f'/api/v1/api-keys/{self.segment(id)}', body=body, query=query, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def rotate_account_api_key(self, id: str, query: T.RotateAccountApiKeyQuery | None = None, *, options: RequestOptions | None = None) -> T.CreatedAccountApiKeyResponse:
        """POST /api/v1/api-keys/{id}/rotate."""
        return cast("T.CreatedAccountApiKeyResponse", self.request('POST', f'/api/v1/api-keys/{self.segment(id)}/rotate', query=query, binary_body=False, binary_response=False, options=options))

    def list_attention(self, query: T.ListAttentionQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_AttentionItemDto:
        """GET /api/v1/attention."""
        return cast("T.Page_AttentionItemDto", self.request('GET', f'/api/v1/attention', query=query, binary_body=False, binary_response=False, options=options))

    def dismiss_attention(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/attention/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/attention/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def list_budgets(self, query: T.ListBudgetsQuery | None = None, *, options: RequestOptions | None = None) -> T.Items_BudgetDto:
        """GET /api/v1/billing/budgets."""
        return cast("T.Items_BudgetDto", self.request('GET', f'/api/v1/billing/budgets', query=query, binary_body=False, binary_response=False, options=options))

    def create_budget(self, body: T.CreateBudgetRequest, *, options: RequestOptions | None = None) -> T.BudgetDto:
        """POST /api/v1/billing/budgets."""
        return cast("T.BudgetDto", self.request('POST', f'/api/v1/billing/budgets', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def delete_budget(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/billing/budgets/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/billing/budgets/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def billing_checkout(self, body: T.CheckoutRequest, *, options: RequestOptions | None = None) -> T.CheckoutResponse:
        """POST /api/v1/billing/checkout."""
        return cast("T.CheckoutResponse", self.request('POST', f'/api/v1/billing/checkout', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def billing_entitlements(self, query: T.BillingEntitlementsQuery | None = None, *, options: RequestOptions | None = None) -> T.EntitlementsDto:
        """GET /api/v1/billing/entitlements."""
        return cast("T.EntitlementsDto", self.request('GET', f'/api/v1/billing/entitlements', query=query, binary_body=False, binary_response=False, options=options))

    def billing_ledger(self, query: T.BillingLedgerQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_LedgerEntryDto:
        """GET /api/v1/billing/ledger."""
        return cast("T.Page_LedgerEntryDto", self.request('GET', f'/api/v1/billing/ledger', query=query, binary_body=False, binary_response=False, options=options))

    def billing_payments(self, query: T.BillingPaymentsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_PaymentDto:
        """GET /api/v1/billing/payments."""
        return cast("T.Page_PaymentDto", self.request('GET', f'/api/v1/billing/payments', query=query, binary_body=False, binary_response=False, options=options))

    def billing_plans(self, *, options: RequestOptions | None = None) -> T.PlansResponse:
        """GET /api/v1/billing/plans."""
        return cast("T.PlansResponse", self.request('GET', f'/api/v1/billing/plans', binary_body=False, binary_response=False, options=options))

    def billing_portal(self, query: T.BillingPortalQuery | None = None, *, options: RequestOptions | None = None) -> T.PortalResponse:
        """POST /api/v1/billing/portal."""
        return cast("T.PortalResponse", self.request('POST', f'/api/v1/billing/portal', query=query, binary_body=False, binary_response=False, options=options))

    def get_subscription(self, query: T.GetSubscriptionQuery | None = None, *, options: RequestOptions | None = None) -> T.SubscriptionDto:
        """GET /api/v1/billing/subscription."""
        return cast("T.SubscriptionDto", self.request('GET', f'/api/v1/billing/subscription', query=query, binary_body=False, binary_response=False, options=options))

    def cancel_subscription(self, query: T.CancelSubscriptionQuery | None = None, *, options: RequestOptions | None = None) -> T.SubscriptionDto:
        """POST /api/v1/billing/subscription/cancel."""
        return cast("T.SubscriptionDto", self.request('POST', f'/api/v1/billing/subscription/cancel', query=query, binary_body=False, binary_response=False, options=options))

    def resume_subscription(self, query: T.ResumeSubscriptionQuery | None = None, *, options: RequestOptions | None = None) -> T.SubscriptionDto:
        """POST /api/v1/billing/subscription/resume."""
        return cast("T.SubscriptionDto", self.request('POST', f'/api/v1/billing/subscription/resume', query=query, binary_body=False, binary_response=False, options=options))

    def billing_usage(self, query: T.BillingUsageQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_UsageItemDto:
        """GET /api/v1/billing/usage."""
        return cast("T.Page_UsageItemDto", self.request('GET', f'/api/v1/billing/usage', query=query, binary_body=False, binary_response=False, options=options))

    def get_wallet(self, query: T.GetWalletQuery | None = None, *, options: RequestOptions | None = None) -> T.WalletDto:
        """GET /api/v1/billing/wallet."""
        return cast("T.WalletDto", self.request('GET', f'/api/v1/billing/wallet', query=query, binary_body=False, binary_response=False, options=options))

    def patch_wallet(self, body: T.UpdateWalletRequest, query: T.PatchWalletQuery | None = None, *, options: RequestOptions | None = None) -> T.WalletDto:
        """PATCH /api/v1/billing/wallet."""
        return cast("T.WalletDto", self.request('PATCH', f'/api/v1/billing/wallet', body=body, query=query, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def capabilities(self, *, options: RequestOptions | None = None) -> T.CapabilitiesResponse:
        """GET /api/v1/capabilities."""
        return cast("T.CapabilitiesResponse", self.request('GET', f'/api/v1/capabilities', binary_body=False, binary_response=False, options=options))

    def list_cloud_agents(self, query: T.ListCloudAgentsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_CloudAgentDto:
        """GET /api/v1/cloud-agents."""
        return cast("T.Page_CloudAgentDto", self.request('GET', f'/api/v1/cloud-agents', query=query, binary_body=False, binary_response=False, options=options))

    def create_cloud_agent(self, body: T.CreateCloudAgentRequest, *, options: RequestOptions | None = None) -> T.CloudAgentDto:
        """POST /api/v1/cloud-agents."""
        return cast("T.CloudAgentDto", self.request('POST', f'/api/v1/cloud-agents', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def get_cloud_agent(self, id: str, *, options: RequestOptions | None = None) -> T.CloudAgentDto:
        """GET /api/v1/cloud-agents/{id}."""
        return cast("T.CloudAgentDto", self.request('GET', f'/api/v1/cloud-agents/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def update_cloud_agent(self, id: str, body: T.UpdateCloudAgentRequest, *, options: RequestOptions | None = None) -> T.CloudAgentDto:
        """PUT /api/v1/cloud-agents/{id}."""
        return cast("T.CloudAgentDto", self.request('PUT', f'/api/v1/cloud-agents/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def delete_cloud_agent(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/cloud-agents/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/cloud-agents/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def csrf(self, *, options: RequestOptions | None = None) -> T.CsrfResponse:
        """GET /api/v1/csrf."""
        return cast("T.CsrfResponse", self.request('GET', f'/api/v1/csrf', binary_body=False, binary_response=False, options=options))

    def device_details(self, userCode: str, *, options: RequestOptions | None = None) -> T.DeviceCodeDetailsResponse:
        """GET /api/v1/device/{userCode}."""
        return cast("T.DeviceCodeDetailsResponse", self.request('GET', f'/api/v1/device/{self.segment(userCode)}', binary_body=False, binary_response=False, options=options))

    def device_approve(self, userCode: str, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/device/{userCode}/approve."""
        return cast("None", self.request('POST', f'/api/v1/device/{self.segment(userCode)}/approve', binary_body=False, binary_response=False, options=options))

    def device_deny(self, userCode: str, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/device/{userCode}/deny."""
        return cast("None", self.request('POST', f'/api/v1/device/{self.segment(userCode)}/deny', binary_body=False, binary_response=False, options=options))

    def list_devices(self, *, options: RequestOptions | None = None) -> T.DevicesResponse:
        """GET /api/v1/devices."""
        return cast("T.DevicesResponse", self.request('GET', f'/api/v1/devices', binary_body=False, binary_response=False, options=options))

    def revoke_device(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/devices/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/devices/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def accept_invitation(self, body: T.AcceptInvitationRequest, *, options: RequestOptions | None = None) -> T.AccountDto:
        """POST /api/v1/invitations/accept."""
        return cast("T.AccountDto", self.request('POST', f'/api/v1/invitations/accept', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def api_key_context(self, *, options: RequestOptions | None = None) -> T.ApiKeyContextDto:
        """GET /api/v1/key."""
        return cast("T.ApiKeyContextDto", self.request('GET', f'/api/v1/key', binary_body=False, binary_response=False, options=options))

    def upload_artifact(self, body: bytes, query: T.UploadArtifactQuery | None = None, *, options: RequestOptions | None = None) -> T.UploadArtifactResponse:
        """POST /api/v1/marketplace/artifacts."""
        return cast("T.UploadArtifactResponse", self.request('POST', f'/api/v1/marketplace/artifacts', body=body, query=query, content_type='application/zip', binary_body=True, binary_response=False, options=options))

    def list_collections(self, query: T.ListCollectionsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_CollectionRefDto:
        """GET /api/v1/marketplace/collections."""
        return cast("T.Page_CollectionRefDto", self.request('GET', f'/api/v1/marketplace/collections', query=query, binary_body=False, binary_response=False, options=options))

    def get_collection(self, slug: str, query: T.GetCollectionQuery | None = None, *, options: RequestOptions | None = None) -> T.CollectionDto:
        """GET /api/v1/marketplace/collections/{slug}."""
        return cast("T.CollectionDto", self.request('GET', f'/api/v1/marketplace/collections/{self.segment(slug)}', query=query, binary_body=False, binary_response=False, options=options))

    def get_publisher(self, slug: str, query: T.GetPublisherQuery | None = None, *, options: RequestOptions | None = None) -> T.PublisherPageDto:
        """GET /api/v1/marketplace/publishers/{slug}."""
        return cast("T.PublisherPageDto", self.request('GET', f'/api/v1/marketplace/publishers/{self.segment(slug)}', query=query, binary_body=False, binary_response=False, options=options))

    def list_resources(self, query: T.ListResourcesQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_ResourceSummaryDto:
        """GET /api/v1/marketplace/resources."""
        return cast("T.Page_ResourceSummaryDto", self.request('GET', f'/api/v1/marketplace/resources', query=query, binary_body=False, binary_response=False, options=options))

    def get_resource(self, id: str, *, options: RequestOptions | None = None) -> T.ResourceDetailDto:
        """GET /api/v1/marketplace/resources/{id}."""
        return cast("T.ResourceDetailDto", self.request('GET', f'/api/v1/marketplace/resources/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def download_marketplace_artifact(self, id: str, query: T.DownloadMarketplaceArtifactQuery | None = None, *, options: RequestOptions | None = None) -> bytes:
        """GET /api/v1/marketplace/resources/{id}/artifact."""
        return cast("bytes", self.request('GET', f'/api/v1/marketplace/resources/{self.segment(id)}/artifact', query=query, binary_body=False, binary_response=True, options=options))

    def install_manifest(self, id: str, query: T.InstallManifestQuery | None = None, *, options: RequestOptions | None = None) -> T.InstallManifestDto:
        """GET /api/v1/marketplace/resources/{id}/install-manifest."""
        return cast("T.InstallManifestDto", self.request('GET', f'/api/v1/marketplace/resources/{self.segment(id)}/install-manifest', query=query, binary_body=False, binary_response=False, options=options))

    def list_resource_versions(self, id: str, *, options: RequestOptions | None = None) -> T.Items_VersionSummaryDto:
        """GET /api/v1/marketplace/resources/{id}/versions."""
        return cast("T.Items_VersionSummaryDto", self.request('GET', f'/api/v1/marketplace/resources/{self.segment(id)}/versions', binary_body=False, binary_response=False, options=options))

    def create_submission(self, body: T.CreateSubmissionRequest, query: T.CreateSubmissionQuery | None = None, *, options: RequestOptions | None = None) -> T.SubmissionDto:
        """POST /api/v1/marketplace/submissions."""
        return cast("T.SubmissionDto", self.request('POST', f'/api/v1/marketplace/submissions', body=body, query=query, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def get_submission(self, id: str, *, options: RequestOptions | None = None) -> T.SubmissionDto:
        """GET /api/v1/marketplace/submissions/{id}."""
        return cast("T.SubmissionDto", self.request('GET', f'/api/v1/marketplace/submissions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def me(self, *, options: RequestOptions | None = None) -> T.MeResponse:
        """GET /api/v1/me."""
        return cast("T.MeResponse", self.request('GET', f'/api/v1/me', binary_body=False, binary_response=False, options=options))

    def account_deletion_preview(self, *, options: RequestOptions | None = None) -> T.AccountDeletionPreviewDto:
        """GET /api/v1/me/deletion."""
        return cast("T.AccountDeletionPreviewDto", self.request('GET', f'/api/v1/me/deletion', binary_body=False, binary_response=False, options=options))

    def account_deletion(self, body: T.AccountDeletionRequest, *, options: RequestOptions | None = None) -> T.AccountDeletionResponse:
        """POST /api/v1/me/deletion."""
        return cast("T.AccountDeletionResponse", self.request('POST', f'/api/v1/me/deletion', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def sign_in_methods(self, *, options: RequestOptions | None = None) -> T.SignInMethodsDto:
        """GET /api/v1/me/sign-in-methods."""
        return cast("T.SignInMethodsDto", self.request('GET', f'/api/v1/me/sign-in-methods', binary_body=False, binary_response=False, options=options))

    def connect_sign_in_method(self, body: T.ConnectSignInMethodRequest, *, options: RequestOptions | None = None) -> T.ConnectSignInMethodResponse:
        """POST /api/v1/me/sign-in-methods/connect."""
        return cast("T.ConnectSignInMethodResponse", self.request('POST', f'/api/v1/me/sign-in-methods/connect', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def remove_sign_in_method(self, id: str, body: T.RemoveSignInMethodRequest, *, options: RequestOptions | None = None) -> T.SignInMethodsDto:
        """POST /api/v1/me/sign-in-methods/{id}/remove."""
        return cast("T.SignInMethodsDto", self.request('POST', f'/api/v1/me/sign-in-methods/{self.segment(id)}/remove', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def mfa_status(self, *, options: RequestOptions | None = None) -> T.MfaStatusDto:
        """GET /api/v1/mfa."""
        return cast("T.MfaStatusDto", self.request('GET', f'/api/v1/mfa', binary_body=False, binary_response=False, options=options))

    def mfa_disable(self, body: T.MfaCodeRequest, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/mfa/disable."""
        return cast("None", self.request('POST', f'/api/v1/mfa/disable', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def mfa_recovery_codes(self, body: T.MfaCodeRequest, *, options: RequestOptions | None = None) -> T.MfaRecoveryCodesResponse:
        """POST /api/v1/mfa/recovery-codes."""
        return cast("T.MfaRecoveryCodesResponse", self.request('POST', f'/api/v1/mfa/recovery-codes', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def mfa_totp_confirm(self, body: T.MfaCodeRequest, *, options: RequestOptions | None = None) -> T.MfaConfirmResponse:
        """POST /api/v1/mfa/totp/confirm."""
        return cast("T.MfaConfirmResponse", self.request('POST', f'/api/v1/mfa/totp/confirm', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def mfa_totp_setup(self, body: T.MfaSetupRequest, *, options: RequestOptions | None = None) -> T.MfaSetupResponse:
        """POST /api/v1/mfa/totp/setup."""
        return cast("T.MfaSetupResponse", self.request('POST', f'/api/v1/mfa/totp/setup', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def mfa_forget_browsers(self, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/mfa/trusted-browsers."""
        return cast("None", self.request('DELETE', f'/api/v1/mfa/trusted-browsers', binary_body=False, binary_response=False, options=options))

    def get_operation(self, id: str, *, options: RequestOptions | None = None) -> T.OperationDto:
        """GET /api/v1/operations/{id}."""
        return cast("T.OperationDto", self.request('GET', f'/api/v1/operations/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def operation_events(self, id: str, query: T.OperationEventsQuery | None = None, *, options: RequestOptions | None = None) -> T.OperationEventsResponse:
        """GET /api/v1/operations/{id}/events."""
        return cast("T.OperationEventsResponse", self.request('GET', f'/api/v1/operations/{self.segment(id)}/events', query=query, binary_body=False, binary_response=False, options=options))

    def list_profiles(self, query: T.ListProfilesQuery | None = None, *, options: RequestOptions | None = None) -> T.Items_ProfileRecordDto:
        """GET /api/v1/profiles."""
        return cast("T.Items_ProfileRecordDto", self.request('GET', f'/api/v1/profiles', query=query, binary_body=False, binary_response=False, options=options))

    def upload_profile(self, body: bytes, query: T.UploadProfileQuery | None = None, *, options: RequestOptions | None = None) -> T.ProfileRecordDto:
        """POST /api/v1/profiles."""
        return cast("T.ProfileRecordDto", self.request('POST', f'/api/v1/profiles', body=body, query=query, content_type='application/zip', binary_body=True, binary_response=False, options=options))

    def get_profile(self, id: str, query: T.GetProfileQuery | None = None, *, options: RequestOptions | None = None) -> T.ProfileRecordDto:
        """GET /api/v1/profiles/{id}."""
        return cast("T.ProfileRecordDto", self.request('GET', f'/api/v1/profiles/{self.segment(id)}', query=query, binary_body=False, binary_response=False, options=options))

    def delete_profile(self, id: str, query: T.DeleteProfileQuery | None = None, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/profiles/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/profiles/{self.segment(id)}', query=query, binary_body=False, binary_response=False, options=options))

    def download_profile(self, id: str, query: T.DownloadProfileQuery | None = None, *, options: RequestOptions | None = None) -> bytes:
        """GET /api/v1/profiles/{id}/download."""
        return cast("bytes", self.request('GET', f'/api/v1/profiles/{self.segment(id)}/download', query=query, binary_body=False, binary_response=True, options=options))

    def get_project(self, id: str, *, options: RequestOptions | None = None) -> T.ProjectDto:
        """GET /api/v1/projects/{id}."""
        return cast("T.ProjectDto", self.request('GET', f'/api/v1/projects/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def delete_project(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/projects/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/projects/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def patch_project(self, id: str, body: T.CreateProjectRequest, *, options: RequestOptions | None = None) -> T.ProjectDto:
        """PATCH /api/v1/projects/{id}."""
        return cast("T.ProjectDto", self.request('PATCH', f'/api/v1/projects/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def register_live_activity(self, activityId: str, body: T.RegisterLiveActivityRequest, *, options: RequestOptions | None = None) -> T.LiveActivityDto:
        """PUT /api/v1/push/activities/{activityId}."""
        return cast("T.LiveActivityDto", self.request('PUT', f'/api/v1/push/activities/{self.segment(activityId)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def delete_live_activity(self, activityId: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/push/activities/{activityId}."""
        return cast("None", self.request('DELETE', f'/api/v1/push/activities/{self.segment(activityId)}', binary_body=False, binary_response=False, options=options))

    def get_push_badge(self, *, options: RequestOptions | None = None) -> T.PushBadgeDto:
        """GET /api/v1/push/badge."""
        return cast("T.PushBadgeDto", self.request('GET', f'/api/v1/push/badge', binary_body=False, binary_response=False, options=options))

    def list_push_devices(self, *, options: RequestOptions | None = None) -> T.PushDevicesResponse:
        """GET /api/v1/push/devices."""
        return cast("T.PushDevicesResponse", self.request('GET', f'/api/v1/push/devices', binary_body=False, binary_response=False, options=options))

    def register_push_device(self, body: T.RegisterPushDeviceRequest, *, options: RequestOptions | None = None) -> T.PushDeviceDto:
        """PUT /api/v1/push/devices/current."""
        return cast("T.PushDeviceDto", self.request('PUT', f'/api/v1/push/devices/current', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def unregister_push_device(self, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/push/devices/current."""
        return cast("None", self.request('DELETE', f'/api/v1/push/devices/current', binary_body=False, binary_response=False, options=options))

    def test_push_device(self, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/push/devices/current/test."""
        return cast("None", self.request('POST', f'/api/v1/push/devices/current/test', binary_body=False, binary_response=False, options=options))

    def remove_push_device(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/push/devices/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/push/devices/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def get_push_preferences(self, *, options: RequestOptions | None = None) -> T.PushPreferencesDto:
        """GET /api/v1/push/preferences."""
        return cast("T.PushPreferencesDto", self.request('GET', f'/api/v1/push/preferences', binary_body=False, binary_response=False, options=options))

    def update_push_preferences(self, body: T.UpdatePushPreferencesRequest, *, options: RequestOptions | None = None) -> T.PushPreferencesDto:
        """PATCH /api/v1/push/preferences."""
        return cast("T.PushPreferencesDto", self.request('PATCH', f'/api/v1/push/preferences', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def current_release(self, query: T.CurrentReleaseQuery | None = None, *, options: RequestOptions | None = None) -> T.ReleaseManifestResponse:
        """GET /api/v1/releases."""
        return cast("T.ReleaseManifestResponse", self.request('GET', f'/api/v1/releases', query=query, binary_body=False, binary_response=False, options=options))

    def download_release_artifact(self, id: str, *, options: RequestOptions | None = None) -> bytes:
        """GET /api/v1/releases/artifacts/{id}."""
        return cast("bytes", self.request('GET', f'/api/v1/releases/artifacts/{self.segment(id)}', binary_body=False, binary_response=True, options=options))

    def release_history(self, query: T.ReleaseHistoryQuery | None = None, *, options: RequestOptions | None = None) -> list[T.ReleasePublication]:
        """GET /api/v1/releases/history."""
        return cast("list[T.ReleasePublication]", self.request('GET', f'/api/v1/releases/history', query=query, binary_body=False, binary_response=False, options=options))

    def chrome_extension_updates(self, channel: str, *, options: RequestOptions | None = None) -> str:
        """GET /api/v1/releases/{channel}/chrome-extension/updates.xml."""
        return cast("str", self.request('GET', f'/api/v1/releases/{self.segment(channel)}/chrome-extension/updates.xml', binary_body=False, binary_response=False, options=options))

    def product_release(self, channel: str, product: str, *, options: RequestOptions | None = None) -> T.ReleaseManifestResponse:
        """GET /api/v1/releases/{channel}/{product}."""
        return cast("T.ReleaseManifestResponse", self.request('GET', f'/api/v1/releases/{self.segment(channel)}/{self.segment(product)}', binary_body=False, binary_response=False, options=options))

    def download_release(self, channel: str, product: str, query: T.DownloadReleaseQuery | None = None, *, options: RequestOptions | None = None) -> bytes:
        """GET /api/v1/releases/{channel}/{product}/download."""
        return cast("bytes", self.request('GET', f'/api/v1/releases/{self.segment(channel)}/{self.segment(product)}/download', query=query, binary_body=False, binary_response=True, options=options))

    def list_remote_hosts(self, query: T.ListRemoteHostsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_RemoteHostDto:
        """GET /api/v1/remote-hosts."""
        return cast("T.Page_RemoteHostDto", self.request('GET', f'/api/v1/remote-hosts', query=query, binary_body=False, binary_response=False, options=options))

    def register_remote_host(self, body: T.RegisterRemoteHostRequest, *, options: RequestOptions | None = None) -> T.RemoteHostDto:
        """POST /api/v1/remote-hosts."""
        return cast("T.RemoteHostDto", self.request('POST', f'/api/v1/remote-hosts', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def get_remote_host(self, id: str, *, options: RequestOptions | None = None) -> T.RemoteHostDto:
        """GET /api/v1/remote-hosts/{id}."""
        return cast("T.RemoteHostDto", self.request('GET', f'/api/v1/remote-hosts/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def revoke_remote_host(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/remote-hosts/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/remote-hosts/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def update_remote_host(self, id: str, body: T.UpdateRemoteHostRequest, *, options: RequestOptions | None = None) -> T.RemoteHostDto:
        """PATCH /api/v1/remote-hosts/{id}."""
        return cast("T.RemoteHostDto", self.request('PATCH', f'/api/v1/remote-hosts/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def publish_remote_session(self, id: str, body: T.PublishRemoteSessionRequest, *, options: RequestOptions | None = None) -> T.RemoteSessionDto:
        """POST /api/v1/remote-hosts/{id}/sessions."""
        return cast("T.RemoteSessionDto", self.request('POST', f'/api/v1/remote-hosts/{self.segment(id)}/sessions', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def list_remote_sessions(self, query: T.ListRemoteSessionsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_RemoteSessionDto:
        """GET /api/v1/remote-sessions."""
        return cast("T.Page_RemoteSessionDto", self.request('GET', f'/api/v1/remote-sessions', query=query, binary_body=False, binary_response=False, options=options))

    def get_remote_session(self, id: str, *, options: RequestOptions | None = None) -> T.RemoteSessionDto:
        """GET /api/v1/remote-sessions/{id}."""
        return cast("T.RemoteSessionDto", self.request('GET', f'/api/v1/remote-sessions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def unpublish_remote_session(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/remote-sessions/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/remote-sessions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def report_ai_output(self, body: T.AiOutputReportRequest, *, options: RequestOptions | None = None) -> T.AiOutputReportDto:
        """POST /api/v1/reports/ai-output."""
        return cast("T.AiOutputReportDto", self.request('POST', f'/api/v1/reports/ai-output', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def list_router_keys(self, query: T.ListRouterKeysQuery | None = None, *, options: RequestOptions | None = None) -> T.Items_RouterKeyDto:
        """GET /api/v1/router/keys."""
        return cast("T.Items_RouterKeyDto", self.request('GET', f'/api/v1/router/keys', query=query, binary_body=False, binary_response=False, options=options))

    def create_router_key(self, body: T.CreateRouterKeyRequest, *, options: RequestOptions | None = None) -> T.CreatedRouterKeyResponse:
        """POST /api/v1/router/keys."""
        return cast("T.CreatedRouterKeyResponse", self.request('POST', f'/api/v1/router/keys', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def revoke_router_key(self, keyId: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/router/keys/{keyId}."""
        return cast("None", self.request('DELETE', f'/api/v1/router/keys/{self.segment(keyId)}', binary_body=False, binary_response=False, options=options))

    def rotate_router_key(self, keyId: str, *, options: RequestOptions | None = None) -> T.CreatedRouterKeyResponse:
        """POST /api/v1/router/keys/{keyId}/rotate."""
        return cast("T.CreatedRouterKeyResponse", self.request('POST', f'/api/v1/router/keys/{self.segment(keyId)}/rotate', binary_body=False, binary_response=False, options=options))

    def model_prices(self, *, options: RequestOptions | None = None) -> T.ModelPricesResponse:
        """GET /api/v1/router/model-prices."""
        return cast("T.ModelPricesResponse", self.request('GET', f'/api/v1/router/model-prices', binary_body=False, binary_response=False, options=options))

    def router_providers(self, *, options: RequestOptions | None = None) -> T.RouterProvidersResponse:
        """GET /api/v1/router/providers."""
        return cast("T.RouterProvidersResponse", self.request('GET', f'/api/v1/router/providers', binary_body=False, binary_response=False, options=options))

    def list_routers(self, query: T.ListRoutersQuery | None = None, *, options: RequestOptions | None = None) -> T.RoutersResponse:
        """GET /api/v1/routers."""
        return cast("T.RoutersResponse", self.request('GET', f'/api/v1/routers', query=query, binary_body=False, binary_response=False, options=options))

    def create_router(self, body: T.CreateRouterRequest, *, options: RequestOptions | None = None) -> T.RouterDto:
        """POST /api/v1/routers."""
        return cast("T.RouterDto", self.request('POST', f'/api/v1/routers', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def get_router(self, id: str, *, options: RequestOptions | None = None) -> T.RouterDto:
        """GET /api/v1/routers/{id}."""
        return cast("T.RouterDto", self.request('GET', f'/api/v1/routers/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def delete_router(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/routers/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/routers/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def update_router(self, id: str, body: T.UpdateRouterRequest, *, options: RequestOptions | None = None) -> T.RouterDto:
        """PATCH /api/v1/routers/{id}."""
        return cast("T.RouterDto", self.request('PATCH', f'/api/v1/routers/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def set_router_credential(self, id: str, provider: str, body: T.SetRouterCredentialRequest, *, options: RequestOptions | None = None) -> T.RouterDto:
        """PUT /api/v1/routers/{id}/credentials/{provider}."""
        return cast("T.RouterDto", self.request('PUT', f'/api/v1/routers/{self.segment(id)}/credentials/{self.segment(provider)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def list_cloud_schedules(self, query: T.ListCloudSchedulesQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_CloudScheduleDto:
        """GET /api/v1/schedules."""
        return cast("T.Page_CloudScheduleDto", self.request('GET', f'/api/v1/schedules', query=query, binary_body=False, binary_response=False, options=options))

    def create_cloud_schedule(self, body: T.CreateCloudScheduleRequest, *, options: RequestOptions | None = None) -> T.CloudScheduleDto:
        """POST /api/v1/schedules."""
        return cast("T.CloudScheduleDto", self.request('POST', f'/api/v1/schedules', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def preview_cloud_schedule(self, query: T.PreviewCloudScheduleQuery | None = None, *, options: RequestOptions | None = None) -> T.CloudSchedulePreviewDto:
        """GET /api/v1/schedules/preview."""
        return cast("T.CloudSchedulePreviewDto", self.request('GET', f'/api/v1/schedules/preview', query=query, binary_body=False, binary_response=False, options=options))

    def get_cloud_schedule(self, id: str, *, options: RequestOptions | None = None) -> T.CloudScheduleDto:
        """GET /api/v1/schedules/{id}."""
        return cast("T.CloudScheduleDto", self.request('GET', f'/api/v1/schedules/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def delete_cloud_schedule(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/schedules/{id}."""
        return cast("None", self.request('DELETE', f'/api/v1/schedules/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def update_cloud_schedule(self, id: str, body: T.UpdateCloudScheduleRequest, *, options: RequestOptions | None = None) -> T.CloudScheduleDto:
        """PATCH /api/v1/schedules/{id}."""
        return cast("T.CloudScheduleDto", self.request('PATCH', f'/api/v1/schedules/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def run_cloud_schedule(self, id: str, *, options: RequestOptions | None = None) -> T.CloudScheduleRunDto:
        """POST /api/v1/schedules/{id}/run."""
        return cast("T.CloudScheduleRunDto", self.request('POST', f'/api/v1/schedules/{self.segment(id)}/run', binary_body=False, binary_response=False, options=options))

    def cloud_schedule_runs(self, id: str, query: T.CloudScheduleRunsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_CloudScheduleRunDto:
        """GET /api/v1/schedules/{id}/runs."""
        return cast("T.Page_CloudScheduleRunDto", self.request('GET', f'/api/v1/schedules/{self.segment(id)}/runs', query=query, binary_body=False, binary_response=False, options=options))

    def list_agent_sessions(self, query: T.ListAgentSessionsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_AgentSessionDto:
        """GET /api/v1/sessions."""
        return cast("T.Page_AgentSessionDto", self.request('GET', f'/api/v1/sessions', query=query, binary_body=False, binary_response=False, options=options))

    def create_agent_session(self, body: T.CreateAgentSessionRequest, *, options: RequestOptions | None = None) -> T.AgentSessionCreatedResponse:
        """POST /api/v1/sessions."""
        return cast("T.AgentSessionCreatedResponse", self.request('POST', f'/api/v1/sessions', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def get_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.AgentSessionDto:
        """GET /api/v1/sessions/{id}."""
        return cast("T.AgentSessionDto", self.request('GET', f'/api/v1/sessions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def delete_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """DELETE /api/v1/sessions/{id}."""
        return cast("T.WorkspaceOperationResponse", self.request('DELETE', f'/api/v1/sessions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def update_agent_session(self, id: str, body: T.UpdateAgentSessionRequest, *, options: RequestOptions | None = None) -> T.AgentSessionDto:
        """PATCH /api/v1/sessions/{id}."""
        return cast("T.AgentSessionDto", self.request('PATCH', f'/api/v1/sessions/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def attach_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.AttachResponse:
        """POST /api/v1/sessions/{id}/attach."""
        return cast("T.AttachResponse", self.request('POST', f'/api/v1/sessions/{self.segment(id)}/attach', binary_body=False, binary_response=False, options=options))

    def save_session_agent(self, id: str, body: T.SaveSessionAgentRequest, *, options: RequestOptions | None = None) -> T.CloudAgentDto:
        """POST /api/v1/sessions/{id}/save-agent."""
        return cast("T.CloudAgentDto", self.request('POST', f'/api/v1/sessions/{self.segment(id)}/save-agent', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def start_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/sessions/{id}/start."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/api/v1/sessions/{self.segment(id)}/start', binary_body=False, binary_response=False, options=options))

    def stop_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/sessions/{id}/stop."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/api/v1/sessions/{self.segment(id)}/stop', binary_body=False, binary_response=False, options=options))

    def usage_summary(self, query: T.UsageSummaryQuery | None = None, *, options: RequestOptions | None = None) -> T.UsageSummaryResponse:
        """GET /api/v1/telemetry/usage."""
        return cast("T.UsageSummaryResponse", self.request('GET', f'/api/v1/telemetry/usage', query=query, binary_body=False, binary_response=False, options=options))

    def report_usage(self, body: T.UsageReportRequest, *, options: RequestOptions | None = None) -> T.UsageReportResponse:
        """POST /api/v1/telemetry/usage."""
        return cast("T.UsageReportResponse", self.request('POST', f'/api/v1/telemetry/usage', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def list_workspaces(self, query: T.ListWorkspacesQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_WorkspaceDto:
        """GET /api/v1/workspaces."""
        return cast("T.Page_WorkspaceDto", self.request('GET', f'/api/v1/workspaces', query=query, binary_body=False, binary_response=False, options=options))

    def create_workspace(self, body: T.CreateWorkspaceRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/api/v1/workspaces', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def workspace_options(self, query: T.WorkspaceOptionsQuery | None = None, *, options: RequestOptions | None = None) -> T.WorkspaceOptionsResponse:
        """GET /api/v1/workspaces/options."""
        return cast("T.WorkspaceOptionsResponse", self.request('GET', f'/api/v1/workspaces/options', query=query, binary_body=False, binary_response=False, options=options))

    def get_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceDto:
        """GET /api/v1/workspaces/{id}."""
        return cast("T.WorkspaceDto", self.request('GET', f'/api/v1/workspaces/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def delete_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """DELETE /api/v1/workspaces/{id}."""
        return cast("T.WorkspaceOperationResponse", self.request('DELETE', f'/api/v1/workspaces/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def patch_workspace(self, id: str, body: T.UpdateWorkspaceRequest, *, options: RequestOptions | None = None) -> T.WorkspaceDto:
        """PATCH /api/v1/workspaces/{id}."""
        return cast("T.WorkspaceDto", self.request('PATCH', f'/api/v1/workspaces/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def attach_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.AttachResponse:
        """POST /api/v1/workspaces/{id}/attach."""
        return cast("T.AttachResponse", self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/attach', binary_body=False, binary_response=False, options=options))

    def export_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/export."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/export', binary_body=False, binary_response=False, options=options))

    def download_export(self, id: str, operationId: str, *, options: RequestOptions | None = None) -> T.ExportDownloadResponse:
        """GET /api/v1/workspaces/{id}/exports/{operationId}/download."""
        return cast("T.ExportDownloadResponse", self.request('GET', f'/api/v1/workspaces/{self.segment(id)}/exports/{self.segment(operationId)}/download', binary_body=False, binary_response=False, options=options))

    def list_workspace_operations(self, id: str, query: T.ListWorkspaceOperationsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_OperationDto:
        """GET /api/v1/workspaces/{id}/operations."""
        return cast("T.Page_OperationDto", self.request('GET', f'/api/v1/workspaces/{self.segment(id)}/operations', query=query, binary_body=False, binary_response=False, options=options))

    def restore_workspace(self, id: str, body: T.RestoreRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/restore."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/restore', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def list_snapshots(self, id: str, *, options: RequestOptions | None = None) -> T.Items_SnapshotDto:
        """GET /api/v1/workspaces/{id}/snapshots."""
        return cast("T.Items_SnapshotDto", self.request('GET', f'/api/v1/workspaces/{self.segment(id)}/snapshots', binary_body=False, binary_response=False, options=options))

    def create_snapshot(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/snapshots."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/snapshots', binary_body=False, binary_response=False, options=options))

    def start_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/start."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/start', binary_body=False, binary_response=False, options=options))

    def stop_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/stop."""
        return cast("T.WorkspaceOperationResponse", self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/stop', binary_body=False, binary_response=False, options=options))

    def revoke_workspace_tickets(self, id: str, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/workspaces/{id}/tickets/revoke."""
        return cast("None", self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/tickets/revoke', binary_body=False, binary_response=False, options=options))

    def apple_callback(self, query: T.AppleCallbackQuery, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """GET /auth/apple/callback."""
        return cast("T.HttpRedirect", self.request('GET', f'/auth/apple/callback', query=query, redirect_response=True, binary_body=False, binary_response=False, options=options))

    def apple_callback_post(self, body: T.AppleCallbackPostBody, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """POST /auth/apple/callback."""
        return cast("T.HttpRedirect", self.request('POST', f'/auth/apple/callback', body=body, content_type='application/x-www-form-urlencoded', redirect_response=True, binary_body=False, binary_response=False, options=options))

    def apple_start(self, query: T.AppleStartQuery | None = None, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """GET /auth/apple/start."""
        return cast("T.HttpRedirect", self.request('GET', f'/auth/apple/start', query=query, redirect_response=True, binary_body=False, binary_response=False, options=options))

    def pending_sign_in(self, *, options: RequestOptions | None = None) -> T.PendingSignInDto:
        """GET /auth/connect."""
        return cast("T.PendingSignInDto", self.request('GET', f'/auth/connect', binary_body=False, binary_response=False, options=options))

    def pending_sign_in_cancel(self, *, options: RequestOptions | None = None) -> None:
        """POST /auth/connect/cancel."""
        return cast("None", self.request('POST', f'/auth/connect/cancel', binary_body=False, binary_response=False, options=options))

    def pending_sign_in_separate(self, *, options: RequestOptions | None = None) -> T.SignInResponse:
        """POST /auth/connect/separate."""
        return cast("T.SignInResponse", self.request('POST', f'/auth/connect/separate', binary_body=False, binary_response=False, options=options))

    def email_confirm(self, body: T.EmailConfirmRequest, *, options: RequestOptions | None = None) -> T.SignInResponse:
        """POST /auth/email/confirm."""
        return cast("T.SignInResponse", self.request('POST', f'/auth/email/confirm', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def email_forgot(self, body: T.EmailAccountRequest, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /auth/email/forgot-password."""
        return cast("T.JsonValue", self.request('POST', f'/auth/email/forgot-password', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def email_link(self, body: T.EmailLinkRequest, *, options: RequestOptions | None = None) -> T.EmailLinkResponse:
        """POST /auth/email/link."""
        return cast("T.EmailLinkResponse", self.request('POST', f'/auth/email/link', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def email_login(self, body: T.EmailLoginRequest, *, options: RequestOptions | None = None) -> T.SignInResponse:
        """POST /auth/email/login."""
        return cast("T.SignInResponse", self.request('POST', f'/auth/email/login', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def email_reset(self, body: T.EmailPasswordRequest, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /auth/email/reset-password."""
        return cast("T.JsonValue", self.request('POST', f'/auth/email/reset-password', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def email_start(self, body: T.EmailAccountRequest, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /auth/email/start."""
        return cast("T.JsonValue", self.request('POST', f'/auth/email/start', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def google_callback(self, query: T.GoogleCallbackQuery, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """GET /auth/google/callback."""
        return cast("T.HttpRedirect", self.request('GET', f'/auth/google/callback', query=query, redirect_response=True, binary_body=False, binary_response=False, options=options))

    def google_start(self, query: T.GoogleStartQuery | None = None, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """GET /auth/google/start."""
        return cast("T.HttpRedirect", self.request('GET', f'/auth/google/start', query=query, redirect_response=True, binary_body=False, binary_response=False, options=options))

    def logout(self, *, options: RequestOptions | None = None) -> None:
        """POST /auth/logout."""
        return cast("None", self.request('POST', f'/auth/logout', binary_body=False, binary_response=False, options=options))

    def logout_all(self, *, options: RequestOptions | None = None) -> None:
        """POST /auth/logout-all."""
        return cast("None", self.request('POST', f'/auth/logout-all', binary_body=False, binary_response=False, options=options))

    def mfa_challenge(self, *, options: RequestOptions | None = None) -> T.MfaChallengeDto:
        """GET /auth/mfa."""
        return cast("T.MfaChallengeDto", self.request('GET', f'/auth/mfa', binary_body=False, binary_response=False, options=options))

    def mfa_cancel(self, *, options: RequestOptions | None = None) -> None:
        """POST /auth/mfa/cancel."""
        return cast("None", self.request('POST', f'/auth/mfa/cancel', binary_body=False, binary_response=False, options=options))

    def mfa_verify(self, body: T.MfaVerifyRequest, *, options: RequestOptions | None = None) -> T.SignInResponse:
        """POST /auth/mfa/verify."""
        return cast("T.SignInResponse", self.request('POST', f'/auth/mfa/verify', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def auth_options(self, *, options: RequestOptions | None = None) -> T.JsonValue:
        """GET /auth/options."""
        return cast("T.JsonValue", self.request('GET', f'/auth/options', binary_body=False, binary_response=False, options=options))

    def device_page(self, query: T.DevicePageQuery | None = None, *, options: RequestOptions | None = None) -> str:
        """GET /device."""
        return cast("str", self.request('GET', f'/device', query=query, text_response=True, binary_body=False, binary_response=False, options=options))

    def healthz(self, *, options: RequestOptions | None = None) -> T.HealthResponse:
        """GET /healthz."""
        return cast("T.HealthResponse", self.request('GET', f'/healthz', binary_body=False, binary_response=False, options=options))

    def release_install_ps1(self, *, options: RequestOptions | None = None) -> str:
        """GET /install.ps1."""
        return cast("str", self.request('GET', f'/install.ps1', text_response=True, binary_body=False, binary_response=False, options=options))

    def release_install_sh(self, *, options: RequestOptions | None = None) -> str:
        """GET /install.sh."""
        return cast("str", self.request('GET', f'/install.sh', text_response=True, binary_body=False, binary_response=False, options=options))

    def internal_dead_jobs(self, *, options: RequestOptions | None = None) -> T.Items_DeadJobDto:
        """GET /internal/ops/jobs/dead."""
        return cast("T.Items_DeadJobDto", self.request('GET', f'/internal/ops/jobs/dead', binary_body=False, binary_response=False, options=options))

    def internal_retry_job(self, id: str, *, options: RequestOptions | None = None) -> T.DeadJobDto:
        """POST /internal/ops/jobs/{id}/retry."""
        return cast("T.DeadJobDto", self.request('POST', f'/internal/ops/jobs/{self.segment(id)}/retry', binary_body=False, binary_response=False, options=options))

    def internal_ledger_check(self, query: T.InternalLedgerCheckQuery, *, options: RequestOptions | None = None) -> T.LedgerCheckResponse:
        """GET /internal/ops/ledger-check."""
        return cast("T.LedgerCheckResponse", self.request('GET', f'/internal/ops/ledger-check', query=query, binary_body=False, binary_response=False, options=options))

    def internal_review_version(self, id: str, body: T.ReviewVersionRequest, *, options: RequestOptions | None = None) -> T.ReviewVersionResponse:
        """POST /internal/ops/marketplace/versions/{id}/review."""
        return cast("T.ReviewVersionResponse", self.request('POST', f'/internal/ops/marketplace/versions/{self.segment(id)}/review', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def internal_ai_output_reports(self, query: T.InternalAiOutputReportsQuery | None = None, *, options: RequestOptions | None = None) -> T.AiOutputReportsOpsResponse:
        """GET /internal/ops/reports/ai-output."""
        return cast("T.AiOutputReportsOpsResponse", self.request('GET', f'/internal/ops/reports/ai-output', query=query, binary_body=False, binary_response=False, options=options))

    def internal_resolve_ai_output_report(self, id: str, body: T.ResolveAiOutputReportRequest, *, options: RequestOptions | None = None) -> T.AiOutputReportOpsDto:
        """POST /internal/ops/reports/ai-output/{id}/resolve."""
        return cast("T.AiOutputReportOpsDto", self.request('POST', f'/internal/ops/reports/ai-output/{self.segment(id)}/resolve', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def internal_usage_summary(self, query: T.InternalUsageSummaryQuery | None = None, *, options: RequestOptions | None = None) -> T.UsageOpsResponse:
        """GET /internal/ops/telemetry/usage."""
        return cast("T.UsageOpsResponse", self.request('GET', f'/internal/ops/telemetry/usage', query=query, binary_body=False, binary_response=False, options=options))

    def internal_mfa_reset(self, id: str, *, options: RequestOptions | None = None) -> None:
        """POST /internal/ops/users/{id}/mfa/reset."""
        return cast("None", self.request('POST', f'/internal/ops/users/{self.segment(id)}/mfa/reset', binary_body=False, binary_response=False, options=options))

    def publish_release(self, body: T.PublishReleaseRequest, *, options: RequestOptions | None = None) -> T.ReleasePublication:
        """POST /internal/releases."""
        return cast("T.ReleasePublication", self.request('POST', f'/internal/releases', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def upload_release_artifact(self, body: bytes, query: T.UploadReleaseArtifactQuery, *, options: RequestOptions | None = None) -> T.ReleaseUpload:
        """POST /internal/releases/artifacts."""
        return cast("T.ReleaseUpload", self.request('POST', f'/internal/releases/artifacts', body=body, query=query, content_type='application/octet-stream', binary_body=True, binary_response=False, options=options))

    def withdraw_release(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /internal/releases/{id}."""
        return cast("None", self.request('DELETE', f'/internal/releases/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    def promote_release(self, id: str, *, options: RequestOptions | None = None) -> T.ReleasePublication:
        """POST /internal/releases/{id}/promote."""
        return cast("T.ReleasePublication", self.request('POST', f'/internal/releases/{self.segment(id)}/promote', binary_body=False, binary_response=False, options=options))

    def internal_drain_bucket(self, body: T.DrainBucketRequest, *, options: RequestOptions | None = None) -> T.DrainBucketResponse:
        """POST /internal/test/drain-bucket."""
        return cast("T.DrainBucketResponse", self.request('POST', f'/internal/test/drain-bucket', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def internal_enqueue_job(self, body: T.EnqueueJobRequest, *, options: RequestOptions | None = None) -> T.EnqueueJobResponse:
        """POST /internal/test/enqueue-job."""
        return cast("T.EnqueueJobResponse", self.request('POST', f'/internal/test/enqueue-job', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def internal_grant(self, body: T.GrantTestRequest, *, options: RequestOptions | None = None) -> T.GrantTestResponse:
        """POST /internal/test/grant."""
        return cast("T.GrantTestResponse", self.request('POST', f'/internal/test/grant', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    def oauth_authorize_get(self, query: T.OauthAuthorizeGetQuery | None = None, *, options: RequestOptions | None = None) -> Union[str, T.HttpRedirect]:
        """GET /oauth/authorize."""
        return cast("Union[str, T.HttpRedirect]", self.request('GET', f'/oauth/authorize', query=query, text_response=True, redirect_response=True, binary_body=False, binary_response=False, options=options))

    def oauth_authorize_post(self, body: T.ConsentDecisionRequest, *, options: RequestOptions | None = None) -> Union[T.ConsentDecisionResponse, T.HttpRedirect]:
        """POST /oauth/authorize."""
        return cast("Union[T.ConsentDecisionResponse, T.HttpRedirect]", self.request('POST', f'/oauth/authorize', body=body, content_type='application/json', redirect_response=True, binary_body=False, binary_response=False, options=options))

    def oauth_device_authorization(self, body: T.OauthDeviceAuthorizationBody, *, options: RequestOptions | None = None) -> T.DeviceAuthorizationResponse:
        """POST /oauth/device_authorization."""
        return cast("T.DeviceAuthorizationResponse", self.request('POST', f'/oauth/device_authorization', body=body, content_type='application/x-www-form-urlencoded', binary_body=False, binary_response=False, options=options))

    def oauth_revoke(self, body: T.OauthRevokeBody, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /oauth/revoke."""
        return cast("T.JsonValue", self.request('POST', f'/oauth/revoke', body=body, content_type='application/x-www-form-urlencoded', binary_body=False, binary_response=False, options=options))

    def oauth_token(self, body: T.OauthTokenBody, *, options: RequestOptions | None = None) -> T.TokenResponse:
        """POST /oauth/token."""
        return cast("T.TokenResponse", self.request('POST', f'/oauth/token', body=body, content_type='application/x-www-form-urlencoded', binary_body=False, binary_response=False, options=options))

    def openapi_document(self, *, options: RequestOptions | None = None) -> T.JsonValue:
        """GET /openapi.json."""
        return cast("T.JsonValue", self.request('GET', f'/openapi.json', binary_body=False, binary_response=False, options=options))

    def readyz(self, *, options: RequestOptions | None = None) -> T.ReadyResponse:
        """GET /readyz."""
        return cast("T.ReadyResponse", self.request('GET', f'/readyz', binary_body=False, binary_response=False, options=options))

    def stripe_webhook(self, body: T.StripeWebhookBody, *, options: RequestOptions | None = None) -> T.StripeWebhookAck:
        """POST /webhooks/stripe."""
        return cast("T.StripeWebhookAck", self.request('POST', f'/webhooks/stripe', body=body, content_type='text/plain', binary_body=False, binary_response=False, options=options))

class AsyncCloudApi(AsyncTransport):
    async def apple_app_site_association(self, *, options: RequestOptions | None = None) -> T.AppleAppSiteAssociation:
        """GET /.well-known/apple-app-site-association."""
        return cast("T.AppleAppSiteAssociation", await self.request('GET', f'/.well-known/apple-app-site-association', binary_body=False, binary_response=False, options=options))

    async def android_asset_links(self, *, options: RequestOptions | None = None) -> list[T.AndroidAssetLinkStatement]:
        """GET /.well-known/assetlinks.json."""
        return cast("list[T.AndroidAssetLinkStatement]", await self.request('GET', f'/.well-known/assetlinks.json', binary_body=False, binary_response=False, options=options))

    async def native_router_session(self, body: T.NativeRouterSessionRequest, *, options: RequestOptions | None = None) -> T.NativeRouterSessionResponse:
        """POST /api/v1/account/router-session."""
        return cast("T.NativeRouterSessionResponse", await self.request('POST', f'/api/v1/account/router-session', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def list_accounts(self, *, options: RequestOptions | None = None) -> T.Items_AccountDto:
        """GET /api/v1/accounts."""
        return cast("T.Items_AccountDto", await self.request('GET', f'/api/v1/accounts', binary_body=False, binary_response=False, options=options))

    async def create_account(self, body: T.CreateAccountRequest, *, options: RequestOptions | None = None) -> T.AccountDto:
        """POST /api/v1/accounts."""
        return cast("T.AccountDto", await self.request('POST', f'/api/v1/accounts', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def get_account(self, id: str, *, options: RequestOptions | None = None) -> T.AccountDto:
        """GET /api/v1/accounts/{id}."""
        return cast("T.AccountDto", await self.request('GET', f'/api/v1/accounts/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def patch_account(self, id: str, body: T.UpdateAccountRequest, *, options: RequestOptions | None = None) -> T.AccountDto:
        """PATCH /api/v1/accounts/{id}."""
        return cast("T.AccountDto", await self.request('PATCH', f'/api/v1/accounts/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def list_invitations(self, id: str, *, options: RequestOptions | None = None) -> T.Items_InvitationDto:
        """GET /api/v1/accounts/{id}/invitations."""
        return cast("T.Items_InvitationDto", await self.request('GET', f'/api/v1/accounts/{self.segment(id)}/invitations', binary_body=False, binary_response=False, options=options))

    async def create_invitation(self, id: str, body: T.CreateInvitationRequest, *, options: RequestOptions | None = None) -> T.CreatedInvitationDto:
        """POST /api/v1/accounts/{id}/invitations."""
        return cast("T.CreatedInvitationDto", await self.request('POST', f'/api/v1/accounts/{self.segment(id)}/invitations', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def revoke_invitation(self, id: str, invitationId: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/accounts/{id}/invitations/{invitationId}."""
        return cast("None", await self.request('DELETE', f'/api/v1/accounts/{self.segment(id)}/invitations/{self.segment(invitationId)}', binary_body=False, binary_response=False, options=options))

    async def list_members(self, id: str, *, options: RequestOptions | None = None) -> T.Items_MemberDto:
        """GET /api/v1/accounts/{id}/members."""
        return cast("T.Items_MemberDto", await self.request('GET', f'/api/v1/accounts/{self.segment(id)}/members', binary_body=False, binary_response=False, options=options))

    async def remove_member(self, id: str, userId: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/accounts/{id}/members/{userId}."""
        return cast("None", await self.request('DELETE', f'/api/v1/accounts/{self.segment(id)}/members/{self.segment(userId)}', binary_body=False, binary_response=False, options=options))

    async def update_member(self, id: str, userId: str, body: T.UpdateMemberRequest, *, options: RequestOptions | None = None) -> None:
        """PATCH /api/v1/accounts/{id}/members/{userId}."""
        return cast("None", await self.request('PATCH', f'/api/v1/accounts/{self.segment(id)}/members/{self.segment(userId)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def list_projects(self, id: str, *, options: RequestOptions | None = None) -> T.Items_ProjectDto:
        """GET /api/v1/accounts/{id}/projects."""
        return cast("T.Items_ProjectDto", await self.request('GET', f'/api/v1/accounts/{self.segment(id)}/projects', binary_body=False, binary_response=False, options=options))

    async def create_project(self, id: str, body: T.CreateProjectRequest, *, options: RequestOptions | None = None) -> T.ProjectDto:
        """POST /api/v1/accounts/{id}/projects."""
        return cast("T.ProjectDto", await self.request('POST', f'/api/v1/accounts/{self.segment(id)}/projects', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def account_seats(self, id: str, *, options: RequestOptions | None = None) -> T.SeatsDto:
        """GET /api/v1/accounts/{id}/seats."""
        return cast("T.SeatsDto", await self.request('GET', f'/api/v1/accounts/{self.segment(id)}/seats', binary_body=False, binary_response=False, options=options))

    async def list_account_api_keys(self, query: T.ListAccountApiKeysQuery | None = None, *, options: RequestOptions | None = None) -> T.Items_AccountApiKeyDto:
        """GET /api/v1/api-keys."""
        return cast("T.Items_AccountApiKeyDto", await self.request('GET', f'/api/v1/api-keys', query=query, binary_body=False, binary_response=False, options=options))

    async def create_account_api_key(self, body: T.CreateAccountApiKeyRequest, *, options: RequestOptions | None = None) -> T.CreatedAccountApiKeyResponse:
        """POST /api/v1/api-keys."""
        return cast("T.CreatedAccountApiKeyResponse", await self.request('POST', f'/api/v1/api-keys', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def revoke_account_api_key(self, id: str, query: T.RevokeAccountApiKeyQuery | None = None, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/api-keys/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/api-keys/{self.segment(id)}', query=query, binary_body=False, binary_response=False, options=options))

    async def update_account_api_key(self, id: str, body: T.UpdateAccountApiKeyRequest, query: T.UpdateAccountApiKeyQuery | None = None, *, options: RequestOptions | None = None) -> T.AccountApiKeyDto:
        """PATCH /api/v1/api-keys/{id}."""
        return cast("T.AccountApiKeyDto", await self.request('PATCH', f'/api/v1/api-keys/{self.segment(id)}', body=body, query=query, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def rotate_account_api_key(self, id: str, query: T.RotateAccountApiKeyQuery | None = None, *, options: RequestOptions | None = None) -> T.CreatedAccountApiKeyResponse:
        """POST /api/v1/api-keys/{id}/rotate."""
        return cast("T.CreatedAccountApiKeyResponse", await self.request('POST', f'/api/v1/api-keys/{self.segment(id)}/rotate', query=query, binary_body=False, binary_response=False, options=options))

    async def list_attention(self, query: T.ListAttentionQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_AttentionItemDto:
        """GET /api/v1/attention."""
        return cast("T.Page_AttentionItemDto", await self.request('GET', f'/api/v1/attention', query=query, binary_body=False, binary_response=False, options=options))

    async def dismiss_attention(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/attention/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/attention/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def list_budgets(self, query: T.ListBudgetsQuery | None = None, *, options: RequestOptions | None = None) -> T.Items_BudgetDto:
        """GET /api/v1/billing/budgets."""
        return cast("T.Items_BudgetDto", await self.request('GET', f'/api/v1/billing/budgets', query=query, binary_body=False, binary_response=False, options=options))

    async def create_budget(self, body: T.CreateBudgetRequest, *, options: RequestOptions | None = None) -> T.BudgetDto:
        """POST /api/v1/billing/budgets."""
        return cast("T.BudgetDto", await self.request('POST', f'/api/v1/billing/budgets', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def delete_budget(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/billing/budgets/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/billing/budgets/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def billing_checkout(self, body: T.CheckoutRequest, *, options: RequestOptions | None = None) -> T.CheckoutResponse:
        """POST /api/v1/billing/checkout."""
        return cast("T.CheckoutResponse", await self.request('POST', f'/api/v1/billing/checkout', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def billing_entitlements(self, query: T.BillingEntitlementsQuery | None = None, *, options: RequestOptions | None = None) -> T.EntitlementsDto:
        """GET /api/v1/billing/entitlements."""
        return cast("T.EntitlementsDto", await self.request('GET', f'/api/v1/billing/entitlements', query=query, binary_body=False, binary_response=False, options=options))

    async def billing_ledger(self, query: T.BillingLedgerQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_LedgerEntryDto:
        """GET /api/v1/billing/ledger."""
        return cast("T.Page_LedgerEntryDto", await self.request('GET', f'/api/v1/billing/ledger', query=query, binary_body=False, binary_response=False, options=options))

    async def billing_payments(self, query: T.BillingPaymentsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_PaymentDto:
        """GET /api/v1/billing/payments."""
        return cast("T.Page_PaymentDto", await self.request('GET', f'/api/v1/billing/payments', query=query, binary_body=False, binary_response=False, options=options))

    async def billing_plans(self, *, options: RequestOptions | None = None) -> T.PlansResponse:
        """GET /api/v1/billing/plans."""
        return cast("T.PlansResponse", await self.request('GET', f'/api/v1/billing/plans', binary_body=False, binary_response=False, options=options))

    async def billing_portal(self, query: T.BillingPortalQuery | None = None, *, options: RequestOptions | None = None) -> T.PortalResponse:
        """POST /api/v1/billing/portal."""
        return cast("T.PortalResponse", await self.request('POST', f'/api/v1/billing/portal', query=query, binary_body=False, binary_response=False, options=options))

    async def get_subscription(self, query: T.GetSubscriptionQuery | None = None, *, options: RequestOptions | None = None) -> T.SubscriptionDto:
        """GET /api/v1/billing/subscription."""
        return cast("T.SubscriptionDto", await self.request('GET', f'/api/v1/billing/subscription', query=query, binary_body=False, binary_response=False, options=options))

    async def cancel_subscription(self, query: T.CancelSubscriptionQuery | None = None, *, options: RequestOptions | None = None) -> T.SubscriptionDto:
        """POST /api/v1/billing/subscription/cancel."""
        return cast("T.SubscriptionDto", await self.request('POST', f'/api/v1/billing/subscription/cancel', query=query, binary_body=False, binary_response=False, options=options))

    async def resume_subscription(self, query: T.ResumeSubscriptionQuery | None = None, *, options: RequestOptions | None = None) -> T.SubscriptionDto:
        """POST /api/v1/billing/subscription/resume."""
        return cast("T.SubscriptionDto", await self.request('POST', f'/api/v1/billing/subscription/resume', query=query, binary_body=False, binary_response=False, options=options))

    async def billing_usage(self, query: T.BillingUsageQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_UsageItemDto:
        """GET /api/v1/billing/usage."""
        return cast("T.Page_UsageItemDto", await self.request('GET', f'/api/v1/billing/usage', query=query, binary_body=False, binary_response=False, options=options))

    async def get_wallet(self, query: T.GetWalletQuery | None = None, *, options: RequestOptions | None = None) -> T.WalletDto:
        """GET /api/v1/billing/wallet."""
        return cast("T.WalletDto", await self.request('GET', f'/api/v1/billing/wallet', query=query, binary_body=False, binary_response=False, options=options))

    async def patch_wallet(self, body: T.UpdateWalletRequest, query: T.PatchWalletQuery | None = None, *, options: RequestOptions | None = None) -> T.WalletDto:
        """PATCH /api/v1/billing/wallet."""
        return cast("T.WalletDto", await self.request('PATCH', f'/api/v1/billing/wallet', body=body, query=query, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def capabilities(self, *, options: RequestOptions | None = None) -> T.CapabilitiesResponse:
        """GET /api/v1/capabilities."""
        return cast("T.CapabilitiesResponse", await self.request('GET', f'/api/v1/capabilities', binary_body=False, binary_response=False, options=options))

    async def list_cloud_agents(self, query: T.ListCloudAgentsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_CloudAgentDto:
        """GET /api/v1/cloud-agents."""
        return cast("T.Page_CloudAgentDto", await self.request('GET', f'/api/v1/cloud-agents', query=query, binary_body=False, binary_response=False, options=options))

    async def create_cloud_agent(self, body: T.CreateCloudAgentRequest, *, options: RequestOptions | None = None) -> T.CloudAgentDto:
        """POST /api/v1/cloud-agents."""
        return cast("T.CloudAgentDto", await self.request('POST', f'/api/v1/cloud-agents', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def get_cloud_agent(self, id: str, *, options: RequestOptions | None = None) -> T.CloudAgentDto:
        """GET /api/v1/cloud-agents/{id}."""
        return cast("T.CloudAgentDto", await self.request('GET', f'/api/v1/cloud-agents/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def update_cloud_agent(self, id: str, body: T.UpdateCloudAgentRequest, *, options: RequestOptions | None = None) -> T.CloudAgentDto:
        """PUT /api/v1/cloud-agents/{id}."""
        return cast("T.CloudAgentDto", await self.request('PUT', f'/api/v1/cloud-agents/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def delete_cloud_agent(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/cloud-agents/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/cloud-agents/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def csrf(self, *, options: RequestOptions | None = None) -> T.CsrfResponse:
        """GET /api/v1/csrf."""
        return cast("T.CsrfResponse", await self.request('GET', f'/api/v1/csrf', binary_body=False, binary_response=False, options=options))

    async def device_details(self, userCode: str, *, options: RequestOptions | None = None) -> T.DeviceCodeDetailsResponse:
        """GET /api/v1/device/{userCode}."""
        return cast("T.DeviceCodeDetailsResponse", await self.request('GET', f'/api/v1/device/{self.segment(userCode)}', binary_body=False, binary_response=False, options=options))

    async def device_approve(self, userCode: str, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/device/{userCode}/approve."""
        return cast("None", await self.request('POST', f'/api/v1/device/{self.segment(userCode)}/approve', binary_body=False, binary_response=False, options=options))

    async def device_deny(self, userCode: str, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/device/{userCode}/deny."""
        return cast("None", await self.request('POST', f'/api/v1/device/{self.segment(userCode)}/deny', binary_body=False, binary_response=False, options=options))

    async def list_devices(self, *, options: RequestOptions | None = None) -> T.DevicesResponse:
        """GET /api/v1/devices."""
        return cast("T.DevicesResponse", await self.request('GET', f'/api/v1/devices', binary_body=False, binary_response=False, options=options))

    async def revoke_device(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/devices/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/devices/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def accept_invitation(self, body: T.AcceptInvitationRequest, *, options: RequestOptions | None = None) -> T.AccountDto:
        """POST /api/v1/invitations/accept."""
        return cast("T.AccountDto", await self.request('POST', f'/api/v1/invitations/accept', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def api_key_context(self, *, options: RequestOptions | None = None) -> T.ApiKeyContextDto:
        """GET /api/v1/key."""
        return cast("T.ApiKeyContextDto", await self.request('GET', f'/api/v1/key', binary_body=False, binary_response=False, options=options))

    async def upload_artifact(self, body: bytes, query: T.UploadArtifactQuery | None = None, *, options: RequestOptions | None = None) -> T.UploadArtifactResponse:
        """POST /api/v1/marketplace/artifacts."""
        return cast("T.UploadArtifactResponse", await self.request('POST', f'/api/v1/marketplace/artifacts', body=body, query=query, content_type='application/zip', binary_body=True, binary_response=False, options=options))

    async def list_collections(self, query: T.ListCollectionsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_CollectionRefDto:
        """GET /api/v1/marketplace/collections."""
        return cast("T.Page_CollectionRefDto", await self.request('GET', f'/api/v1/marketplace/collections', query=query, binary_body=False, binary_response=False, options=options))

    async def get_collection(self, slug: str, query: T.GetCollectionQuery | None = None, *, options: RequestOptions | None = None) -> T.CollectionDto:
        """GET /api/v1/marketplace/collections/{slug}."""
        return cast("T.CollectionDto", await self.request('GET', f'/api/v1/marketplace/collections/{self.segment(slug)}', query=query, binary_body=False, binary_response=False, options=options))

    async def get_publisher(self, slug: str, query: T.GetPublisherQuery | None = None, *, options: RequestOptions | None = None) -> T.PublisherPageDto:
        """GET /api/v1/marketplace/publishers/{slug}."""
        return cast("T.PublisherPageDto", await self.request('GET', f'/api/v1/marketplace/publishers/{self.segment(slug)}', query=query, binary_body=False, binary_response=False, options=options))

    async def list_resources(self, query: T.ListResourcesQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_ResourceSummaryDto:
        """GET /api/v1/marketplace/resources."""
        return cast("T.Page_ResourceSummaryDto", await self.request('GET', f'/api/v1/marketplace/resources', query=query, binary_body=False, binary_response=False, options=options))

    async def get_resource(self, id: str, *, options: RequestOptions | None = None) -> T.ResourceDetailDto:
        """GET /api/v1/marketplace/resources/{id}."""
        return cast("T.ResourceDetailDto", await self.request('GET', f'/api/v1/marketplace/resources/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def download_marketplace_artifact(self, id: str, query: T.DownloadMarketplaceArtifactQuery | None = None, *, options: RequestOptions | None = None) -> bytes:
        """GET /api/v1/marketplace/resources/{id}/artifact."""
        return cast("bytes", await self.request('GET', f'/api/v1/marketplace/resources/{self.segment(id)}/artifact', query=query, binary_body=False, binary_response=True, options=options))

    async def install_manifest(self, id: str, query: T.InstallManifestQuery | None = None, *, options: RequestOptions | None = None) -> T.InstallManifestDto:
        """GET /api/v1/marketplace/resources/{id}/install-manifest."""
        return cast("T.InstallManifestDto", await self.request('GET', f'/api/v1/marketplace/resources/{self.segment(id)}/install-manifest', query=query, binary_body=False, binary_response=False, options=options))

    async def list_resource_versions(self, id: str, *, options: RequestOptions | None = None) -> T.Items_VersionSummaryDto:
        """GET /api/v1/marketplace/resources/{id}/versions."""
        return cast("T.Items_VersionSummaryDto", await self.request('GET', f'/api/v1/marketplace/resources/{self.segment(id)}/versions', binary_body=False, binary_response=False, options=options))

    async def create_submission(self, body: T.CreateSubmissionRequest, query: T.CreateSubmissionQuery | None = None, *, options: RequestOptions | None = None) -> T.SubmissionDto:
        """POST /api/v1/marketplace/submissions."""
        return cast("T.SubmissionDto", await self.request('POST', f'/api/v1/marketplace/submissions', body=body, query=query, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def get_submission(self, id: str, *, options: RequestOptions | None = None) -> T.SubmissionDto:
        """GET /api/v1/marketplace/submissions/{id}."""
        return cast("T.SubmissionDto", await self.request('GET', f'/api/v1/marketplace/submissions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def me(self, *, options: RequestOptions | None = None) -> T.MeResponse:
        """GET /api/v1/me."""
        return cast("T.MeResponse", await self.request('GET', f'/api/v1/me', binary_body=False, binary_response=False, options=options))

    async def account_deletion_preview(self, *, options: RequestOptions | None = None) -> T.AccountDeletionPreviewDto:
        """GET /api/v1/me/deletion."""
        return cast("T.AccountDeletionPreviewDto", await self.request('GET', f'/api/v1/me/deletion', binary_body=False, binary_response=False, options=options))

    async def account_deletion(self, body: T.AccountDeletionRequest, *, options: RequestOptions | None = None) -> T.AccountDeletionResponse:
        """POST /api/v1/me/deletion."""
        return cast("T.AccountDeletionResponse", await self.request('POST', f'/api/v1/me/deletion', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def sign_in_methods(self, *, options: RequestOptions | None = None) -> T.SignInMethodsDto:
        """GET /api/v1/me/sign-in-methods."""
        return cast("T.SignInMethodsDto", await self.request('GET', f'/api/v1/me/sign-in-methods', binary_body=False, binary_response=False, options=options))

    async def connect_sign_in_method(self, body: T.ConnectSignInMethodRequest, *, options: RequestOptions | None = None) -> T.ConnectSignInMethodResponse:
        """POST /api/v1/me/sign-in-methods/connect."""
        return cast("T.ConnectSignInMethodResponse", await self.request('POST', f'/api/v1/me/sign-in-methods/connect', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def remove_sign_in_method(self, id: str, body: T.RemoveSignInMethodRequest, *, options: RequestOptions | None = None) -> T.SignInMethodsDto:
        """POST /api/v1/me/sign-in-methods/{id}/remove."""
        return cast("T.SignInMethodsDto", await self.request('POST', f'/api/v1/me/sign-in-methods/{self.segment(id)}/remove', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def mfa_status(self, *, options: RequestOptions | None = None) -> T.MfaStatusDto:
        """GET /api/v1/mfa."""
        return cast("T.MfaStatusDto", await self.request('GET', f'/api/v1/mfa', binary_body=False, binary_response=False, options=options))

    async def mfa_disable(self, body: T.MfaCodeRequest, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/mfa/disable."""
        return cast("None", await self.request('POST', f'/api/v1/mfa/disable', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def mfa_recovery_codes(self, body: T.MfaCodeRequest, *, options: RequestOptions | None = None) -> T.MfaRecoveryCodesResponse:
        """POST /api/v1/mfa/recovery-codes."""
        return cast("T.MfaRecoveryCodesResponse", await self.request('POST', f'/api/v1/mfa/recovery-codes', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def mfa_totp_confirm(self, body: T.MfaCodeRequest, *, options: RequestOptions | None = None) -> T.MfaConfirmResponse:
        """POST /api/v1/mfa/totp/confirm."""
        return cast("T.MfaConfirmResponse", await self.request('POST', f'/api/v1/mfa/totp/confirm', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def mfa_totp_setup(self, body: T.MfaSetupRequest, *, options: RequestOptions | None = None) -> T.MfaSetupResponse:
        """POST /api/v1/mfa/totp/setup."""
        return cast("T.MfaSetupResponse", await self.request('POST', f'/api/v1/mfa/totp/setup', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def mfa_forget_browsers(self, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/mfa/trusted-browsers."""
        return cast("None", await self.request('DELETE', f'/api/v1/mfa/trusted-browsers', binary_body=False, binary_response=False, options=options))

    async def get_operation(self, id: str, *, options: RequestOptions | None = None) -> T.OperationDto:
        """GET /api/v1/operations/{id}."""
        return cast("T.OperationDto", await self.request('GET', f'/api/v1/operations/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def operation_events(self, id: str, query: T.OperationEventsQuery | None = None, *, options: RequestOptions | None = None) -> T.OperationEventsResponse:
        """GET /api/v1/operations/{id}/events."""
        return cast("T.OperationEventsResponse", await self.request('GET', f'/api/v1/operations/{self.segment(id)}/events', query=query, binary_body=False, binary_response=False, options=options))

    async def list_profiles(self, query: T.ListProfilesQuery | None = None, *, options: RequestOptions | None = None) -> T.Items_ProfileRecordDto:
        """GET /api/v1/profiles."""
        return cast("T.Items_ProfileRecordDto", await self.request('GET', f'/api/v1/profiles', query=query, binary_body=False, binary_response=False, options=options))

    async def upload_profile(self, body: bytes, query: T.UploadProfileQuery | None = None, *, options: RequestOptions | None = None) -> T.ProfileRecordDto:
        """POST /api/v1/profiles."""
        return cast("T.ProfileRecordDto", await self.request('POST', f'/api/v1/profiles', body=body, query=query, content_type='application/zip', binary_body=True, binary_response=False, options=options))

    async def get_profile(self, id: str, query: T.GetProfileQuery | None = None, *, options: RequestOptions | None = None) -> T.ProfileRecordDto:
        """GET /api/v1/profiles/{id}."""
        return cast("T.ProfileRecordDto", await self.request('GET', f'/api/v1/profiles/{self.segment(id)}', query=query, binary_body=False, binary_response=False, options=options))

    async def delete_profile(self, id: str, query: T.DeleteProfileQuery | None = None, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/profiles/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/profiles/{self.segment(id)}', query=query, binary_body=False, binary_response=False, options=options))

    async def download_profile(self, id: str, query: T.DownloadProfileQuery | None = None, *, options: RequestOptions | None = None) -> bytes:
        """GET /api/v1/profiles/{id}/download."""
        return cast("bytes", await self.request('GET', f'/api/v1/profiles/{self.segment(id)}/download', query=query, binary_body=False, binary_response=True, options=options))

    async def get_project(self, id: str, *, options: RequestOptions | None = None) -> T.ProjectDto:
        """GET /api/v1/projects/{id}."""
        return cast("T.ProjectDto", await self.request('GET', f'/api/v1/projects/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def delete_project(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/projects/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/projects/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def patch_project(self, id: str, body: T.CreateProjectRequest, *, options: RequestOptions | None = None) -> T.ProjectDto:
        """PATCH /api/v1/projects/{id}."""
        return cast("T.ProjectDto", await self.request('PATCH', f'/api/v1/projects/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def register_live_activity(self, activityId: str, body: T.RegisterLiveActivityRequest, *, options: RequestOptions | None = None) -> T.LiveActivityDto:
        """PUT /api/v1/push/activities/{activityId}."""
        return cast("T.LiveActivityDto", await self.request('PUT', f'/api/v1/push/activities/{self.segment(activityId)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def delete_live_activity(self, activityId: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/push/activities/{activityId}."""
        return cast("None", await self.request('DELETE', f'/api/v1/push/activities/{self.segment(activityId)}', binary_body=False, binary_response=False, options=options))

    async def get_push_badge(self, *, options: RequestOptions | None = None) -> T.PushBadgeDto:
        """GET /api/v1/push/badge."""
        return cast("T.PushBadgeDto", await self.request('GET', f'/api/v1/push/badge', binary_body=False, binary_response=False, options=options))

    async def list_push_devices(self, *, options: RequestOptions | None = None) -> T.PushDevicesResponse:
        """GET /api/v1/push/devices."""
        return cast("T.PushDevicesResponse", await self.request('GET', f'/api/v1/push/devices', binary_body=False, binary_response=False, options=options))

    async def register_push_device(self, body: T.RegisterPushDeviceRequest, *, options: RequestOptions | None = None) -> T.PushDeviceDto:
        """PUT /api/v1/push/devices/current."""
        return cast("T.PushDeviceDto", await self.request('PUT', f'/api/v1/push/devices/current', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def unregister_push_device(self, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/push/devices/current."""
        return cast("None", await self.request('DELETE', f'/api/v1/push/devices/current', binary_body=False, binary_response=False, options=options))

    async def test_push_device(self, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/push/devices/current/test."""
        return cast("None", await self.request('POST', f'/api/v1/push/devices/current/test', binary_body=False, binary_response=False, options=options))

    async def remove_push_device(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/push/devices/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/push/devices/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def get_push_preferences(self, *, options: RequestOptions | None = None) -> T.PushPreferencesDto:
        """GET /api/v1/push/preferences."""
        return cast("T.PushPreferencesDto", await self.request('GET', f'/api/v1/push/preferences', binary_body=False, binary_response=False, options=options))

    async def update_push_preferences(self, body: T.UpdatePushPreferencesRequest, *, options: RequestOptions | None = None) -> T.PushPreferencesDto:
        """PATCH /api/v1/push/preferences."""
        return cast("T.PushPreferencesDto", await self.request('PATCH', f'/api/v1/push/preferences', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def current_release(self, query: T.CurrentReleaseQuery | None = None, *, options: RequestOptions | None = None) -> T.ReleaseManifestResponse:
        """GET /api/v1/releases."""
        return cast("T.ReleaseManifestResponse", await self.request('GET', f'/api/v1/releases', query=query, binary_body=False, binary_response=False, options=options))

    async def download_release_artifact(self, id: str, *, options: RequestOptions | None = None) -> bytes:
        """GET /api/v1/releases/artifacts/{id}."""
        return cast("bytes", await self.request('GET', f'/api/v1/releases/artifacts/{self.segment(id)}', binary_body=False, binary_response=True, options=options))

    async def release_history(self, query: T.ReleaseHistoryQuery | None = None, *, options: RequestOptions | None = None) -> list[T.ReleasePublication]:
        """GET /api/v1/releases/history."""
        return cast("list[T.ReleasePublication]", await self.request('GET', f'/api/v1/releases/history', query=query, binary_body=False, binary_response=False, options=options))

    async def chrome_extension_updates(self, channel: str, *, options: RequestOptions | None = None) -> str:
        """GET /api/v1/releases/{channel}/chrome-extension/updates.xml."""
        return cast("str", await self.request('GET', f'/api/v1/releases/{self.segment(channel)}/chrome-extension/updates.xml', binary_body=False, binary_response=False, options=options))

    async def product_release(self, channel: str, product: str, *, options: RequestOptions | None = None) -> T.ReleaseManifestResponse:
        """GET /api/v1/releases/{channel}/{product}."""
        return cast("T.ReleaseManifestResponse", await self.request('GET', f'/api/v1/releases/{self.segment(channel)}/{self.segment(product)}', binary_body=False, binary_response=False, options=options))

    async def download_release(self, channel: str, product: str, query: T.DownloadReleaseQuery | None = None, *, options: RequestOptions | None = None) -> bytes:
        """GET /api/v1/releases/{channel}/{product}/download."""
        return cast("bytes", await self.request('GET', f'/api/v1/releases/{self.segment(channel)}/{self.segment(product)}/download', query=query, binary_body=False, binary_response=True, options=options))

    async def list_remote_hosts(self, query: T.ListRemoteHostsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_RemoteHostDto:
        """GET /api/v1/remote-hosts."""
        return cast("T.Page_RemoteHostDto", await self.request('GET', f'/api/v1/remote-hosts', query=query, binary_body=False, binary_response=False, options=options))

    async def register_remote_host(self, body: T.RegisterRemoteHostRequest, *, options: RequestOptions | None = None) -> T.RemoteHostDto:
        """POST /api/v1/remote-hosts."""
        return cast("T.RemoteHostDto", await self.request('POST', f'/api/v1/remote-hosts', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def get_remote_host(self, id: str, *, options: RequestOptions | None = None) -> T.RemoteHostDto:
        """GET /api/v1/remote-hosts/{id}."""
        return cast("T.RemoteHostDto", await self.request('GET', f'/api/v1/remote-hosts/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def revoke_remote_host(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/remote-hosts/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/remote-hosts/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def update_remote_host(self, id: str, body: T.UpdateRemoteHostRequest, *, options: RequestOptions | None = None) -> T.RemoteHostDto:
        """PATCH /api/v1/remote-hosts/{id}."""
        return cast("T.RemoteHostDto", await self.request('PATCH', f'/api/v1/remote-hosts/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def publish_remote_session(self, id: str, body: T.PublishRemoteSessionRequest, *, options: RequestOptions | None = None) -> T.RemoteSessionDto:
        """POST /api/v1/remote-hosts/{id}/sessions."""
        return cast("T.RemoteSessionDto", await self.request('POST', f'/api/v1/remote-hosts/{self.segment(id)}/sessions', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def list_remote_sessions(self, query: T.ListRemoteSessionsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_RemoteSessionDto:
        """GET /api/v1/remote-sessions."""
        return cast("T.Page_RemoteSessionDto", await self.request('GET', f'/api/v1/remote-sessions', query=query, binary_body=False, binary_response=False, options=options))

    async def get_remote_session(self, id: str, *, options: RequestOptions | None = None) -> T.RemoteSessionDto:
        """GET /api/v1/remote-sessions/{id}."""
        return cast("T.RemoteSessionDto", await self.request('GET', f'/api/v1/remote-sessions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def unpublish_remote_session(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/remote-sessions/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/remote-sessions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def report_ai_output(self, body: T.AiOutputReportRequest, *, options: RequestOptions | None = None) -> T.AiOutputReportDto:
        """POST /api/v1/reports/ai-output."""
        return cast("T.AiOutputReportDto", await self.request('POST', f'/api/v1/reports/ai-output', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def list_router_keys(self, query: T.ListRouterKeysQuery | None = None, *, options: RequestOptions | None = None) -> T.Items_RouterKeyDto:
        """GET /api/v1/router/keys."""
        return cast("T.Items_RouterKeyDto", await self.request('GET', f'/api/v1/router/keys', query=query, binary_body=False, binary_response=False, options=options))

    async def create_router_key(self, body: T.CreateRouterKeyRequest, *, options: RequestOptions | None = None) -> T.CreatedRouterKeyResponse:
        """POST /api/v1/router/keys."""
        return cast("T.CreatedRouterKeyResponse", await self.request('POST', f'/api/v1/router/keys', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def revoke_router_key(self, keyId: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/router/keys/{keyId}."""
        return cast("None", await self.request('DELETE', f'/api/v1/router/keys/{self.segment(keyId)}', binary_body=False, binary_response=False, options=options))

    async def rotate_router_key(self, keyId: str, *, options: RequestOptions | None = None) -> T.CreatedRouterKeyResponse:
        """POST /api/v1/router/keys/{keyId}/rotate."""
        return cast("T.CreatedRouterKeyResponse", await self.request('POST', f'/api/v1/router/keys/{self.segment(keyId)}/rotate', binary_body=False, binary_response=False, options=options))

    async def model_prices(self, *, options: RequestOptions | None = None) -> T.ModelPricesResponse:
        """GET /api/v1/router/model-prices."""
        return cast("T.ModelPricesResponse", await self.request('GET', f'/api/v1/router/model-prices', binary_body=False, binary_response=False, options=options))

    async def router_providers(self, *, options: RequestOptions | None = None) -> T.RouterProvidersResponse:
        """GET /api/v1/router/providers."""
        return cast("T.RouterProvidersResponse", await self.request('GET', f'/api/v1/router/providers', binary_body=False, binary_response=False, options=options))

    async def list_routers(self, query: T.ListRoutersQuery | None = None, *, options: RequestOptions | None = None) -> T.RoutersResponse:
        """GET /api/v1/routers."""
        return cast("T.RoutersResponse", await self.request('GET', f'/api/v1/routers', query=query, binary_body=False, binary_response=False, options=options))

    async def create_router(self, body: T.CreateRouterRequest, *, options: RequestOptions | None = None) -> T.RouterDto:
        """POST /api/v1/routers."""
        return cast("T.RouterDto", await self.request('POST', f'/api/v1/routers', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def get_router(self, id: str, *, options: RequestOptions | None = None) -> T.RouterDto:
        """GET /api/v1/routers/{id}."""
        return cast("T.RouterDto", await self.request('GET', f'/api/v1/routers/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def delete_router(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/routers/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/routers/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def update_router(self, id: str, body: T.UpdateRouterRequest, *, options: RequestOptions | None = None) -> T.RouterDto:
        """PATCH /api/v1/routers/{id}."""
        return cast("T.RouterDto", await self.request('PATCH', f'/api/v1/routers/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def set_router_credential(self, id: str, provider: str, body: T.SetRouterCredentialRequest, *, options: RequestOptions | None = None) -> T.RouterDto:
        """PUT /api/v1/routers/{id}/credentials/{provider}."""
        return cast("T.RouterDto", await self.request('PUT', f'/api/v1/routers/{self.segment(id)}/credentials/{self.segment(provider)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def list_cloud_schedules(self, query: T.ListCloudSchedulesQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_CloudScheduleDto:
        """GET /api/v1/schedules."""
        return cast("T.Page_CloudScheduleDto", await self.request('GET', f'/api/v1/schedules', query=query, binary_body=False, binary_response=False, options=options))

    async def create_cloud_schedule(self, body: T.CreateCloudScheduleRequest, *, options: RequestOptions | None = None) -> T.CloudScheduleDto:
        """POST /api/v1/schedules."""
        return cast("T.CloudScheduleDto", await self.request('POST', f'/api/v1/schedules', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def preview_cloud_schedule(self, query: T.PreviewCloudScheduleQuery | None = None, *, options: RequestOptions | None = None) -> T.CloudSchedulePreviewDto:
        """GET /api/v1/schedules/preview."""
        return cast("T.CloudSchedulePreviewDto", await self.request('GET', f'/api/v1/schedules/preview', query=query, binary_body=False, binary_response=False, options=options))

    async def get_cloud_schedule(self, id: str, *, options: RequestOptions | None = None) -> T.CloudScheduleDto:
        """GET /api/v1/schedules/{id}."""
        return cast("T.CloudScheduleDto", await self.request('GET', f'/api/v1/schedules/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def delete_cloud_schedule(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /api/v1/schedules/{id}."""
        return cast("None", await self.request('DELETE', f'/api/v1/schedules/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def update_cloud_schedule(self, id: str, body: T.UpdateCloudScheduleRequest, *, options: RequestOptions | None = None) -> T.CloudScheduleDto:
        """PATCH /api/v1/schedules/{id}."""
        return cast("T.CloudScheduleDto", await self.request('PATCH', f'/api/v1/schedules/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def run_cloud_schedule(self, id: str, *, options: RequestOptions | None = None) -> T.CloudScheduleRunDto:
        """POST /api/v1/schedules/{id}/run."""
        return cast("T.CloudScheduleRunDto", await self.request('POST', f'/api/v1/schedules/{self.segment(id)}/run', binary_body=False, binary_response=False, options=options))

    async def cloud_schedule_runs(self, id: str, query: T.CloudScheduleRunsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_CloudScheduleRunDto:
        """GET /api/v1/schedules/{id}/runs."""
        return cast("T.Page_CloudScheduleRunDto", await self.request('GET', f'/api/v1/schedules/{self.segment(id)}/runs', query=query, binary_body=False, binary_response=False, options=options))

    async def list_agent_sessions(self, query: T.ListAgentSessionsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_AgentSessionDto:
        """GET /api/v1/sessions."""
        return cast("T.Page_AgentSessionDto", await self.request('GET', f'/api/v1/sessions', query=query, binary_body=False, binary_response=False, options=options))

    async def create_agent_session(self, body: T.CreateAgentSessionRequest, *, options: RequestOptions | None = None) -> T.AgentSessionCreatedResponse:
        """POST /api/v1/sessions."""
        return cast("T.AgentSessionCreatedResponse", await self.request('POST', f'/api/v1/sessions', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def get_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.AgentSessionDto:
        """GET /api/v1/sessions/{id}."""
        return cast("T.AgentSessionDto", await self.request('GET', f'/api/v1/sessions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def delete_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """DELETE /api/v1/sessions/{id}."""
        return cast("T.WorkspaceOperationResponse", await self.request('DELETE', f'/api/v1/sessions/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def update_agent_session(self, id: str, body: T.UpdateAgentSessionRequest, *, options: RequestOptions | None = None) -> T.AgentSessionDto:
        """PATCH /api/v1/sessions/{id}."""
        return cast("T.AgentSessionDto", await self.request('PATCH', f'/api/v1/sessions/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def attach_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.AttachResponse:
        """POST /api/v1/sessions/{id}/attach."""
        return cast("T.AttachResponse", await self.request('POST', f'/api/v1/sessions/{self.segment(id)}/attach', binary_body=False, binary_response=False, options=options))

    async def save_session_agent(self, id: str, body: T.SaveSessionAgentRequest, *, options: RequestOptions | None = None) -> T.CloudAgentDto:
        """POST /api/v1/sessions/{id}/save-agent."""
        return cast("T.CloudAgentDto", await self.request('POST', f'/api/v1/sessions/{self.segment(id)}/save-agent', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def start_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/sessions/{id}/start."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/api/v1/sessions/{self.segment(id)}/start', binary_body=False, binary_response=False, options=options))

    async def stop_agent_session(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/sessions/{id}/stop."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/api/v1/sessions/{self.segment(id)}/stop', binary_body=False, binary_response=False, options=options))

    async def usage_summary(self, query: T.UsageSummaryQuery | None = None, *, options: RequestOptions | None = None) -> T.UsageSummaryResponse:
        """GET /api/v1/telemetry/usage."""
        return cast("T.UsageSummaryResponse", await self.request('GET', f'/api/v1/telemetry/usage', query=query, binary_body=False, binary_response=False, options=options))

    async def report_usage(self, body: T.UsageReportRequest, *, options: RequestOptions | None = None) -> T.UsageReportResponse:
        """POST /api/v1/telemetry/usage."""
        return cast("T.UsageReportResponse", await self.request('POST', f'/api/v1/telemetry/usage', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def list_workspaces(self, query: T.ListWorkspacesQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_WorkspaceDto:
        """GET /api/v1/workspaces."""
        return cast("T.Page_WorkspaceDto", await self.request('GET', f'/api/v1/workspaces', query=query, binary_body=False, binary_response=False, options=options))

    async def create_workspace(self, body: T.CreateWorkspaceRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/api/v1/workspaces', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def workspace_options(self, query: T.WorkspaceOptionsQuery | None = None, *, options: RequestOptions | None = None) -> T.WorkspaceOptionsResponse:
        """GET /api/v1/workspaces/options."""
        return cast("T.WorkspaceOptionsResponse", await self.request('GET', f'/api/v1/workspaces/options', query=query, binary_body=False, binary_response=False, options=options))

    async def get_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceDto:
        """GET /api/v1/workspaces/{id}."""
        return cast("T.WorkspaceDto", await self.request('GET', f'/api/v1/workspaces/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def delete_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """DELETE /api/v1/workspaces/{id}."""
        return cast("T.WorkspaceOperationResponse", await self.request('DELETE', f'/api/v1/workspaces/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def patch_workspace(self, id: str, body: T.UpdateWorkspaceRequest, *, options: RequestOptions | None = None) -> T.WorkspaceDto:
        """PATCH /api/v1/workspaces/{id}."""
        return cast("T.WorkspaceDto", await self.request('PATCH', f'/api/v1/workspaces/{self.segment(id)}', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def attach_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.AttachResponse:
        """POST /api/v1/workspaces/{id}/attach."""
        return cast("T.AttachResponse", await self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/attach', binary_body=False, binary_response=False, options=options))

    async def export_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/export."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/export', binary_body=False, binary_response=False, options=options))

    async def download_export(self, id: str, operationId: str, *, options: RequestOptions | None = None) -> T.ExportDownloadResponse:
        """GET /api/v1/workspaces/{id}/exports/{operationId}/download."""
        return cast("T.ExportDownloadResponse", await self.request('GET', f'/api/v1/workspaces/{self.segment(id)}/exports/{self.segment(operationId)}/download', binary_body=False, binary_response=False, options=options))

    async def list_workspace_operations(self, id: str, query: T.ListWorkspaceOperationsQuery | None = None, *, options: RequestOptions | None = None) -> T.Page_OperationDto:
        """GET /api/v1/workspaces/{id}/operations."""
        return cast("T.Page_OperationDto", await self.request('GET', f'/api/v1/workspaces/{self.segment(id)}/operations', query=query, binary_body=False, binary_response=False, options=options))

    async def restore_workspace(self, id: str, body: T.RestoreRequest, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/restore."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/restore', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def list_snapshots(self, id: str, *, options: RequestOptions | None = None) -> T.Items_SnapshotDto:
        """GET /api/v1/workspaces/{id}/snapshots."""
        return cast("T.Items_SnapshotDto", await self.request('GET', f'/api/v1/workspaces/{self.segment(id)}/snapshots', binary_body=False, binary_response=False, options=options))

    async def create_snapshot(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/snapshots."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/snapshots', binary_body=False, binary_response=False, options=options))

    async def start_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/start."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/start', binary_body=False, binary_response=False, options=options))

    async def stop_workspace(self, id: str, *, options: RequestOptions | None = None) -> T.WorkspaceOperationResponse:
        """POST /api/v1/workspaces/{id}/stop."""
        return cast("T.WorkspaceOperationResponse", await self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/stop', binary_body=False, binary_response=False, options=options))

    async def revoke_workspace_tickets(self, id: str, *, options: RequestOptions | None = None) -> None:
        """POST /api/v1/workspaces/{id}/tickets/revoke."""
        return cast("None", await self.request('POST', f'/api/v1/workspaces/{self.segment(id)}/tickets/revoke', binary_body=False, binary_response=False, options=options))

    async def apple_callback(self, query: T.AppleCallbackQuery, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """GET /auth/apple/callback."""
        return cast("T.HttpRedirect", await self.request('GET', f'/auth/apple/callback', query=query, redirect_response=True, binary_body=False, binary_response=False, options=options))

    async def apple_callback_post(self, body: T.AppleCallbackPostBody, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """POST /auth/apple/callback."""
        return cast("T.HttpRedirect", await self.request('POST', f'/auth/apple/callback', body=body, content_type='application/x-www-form-urlencoded', redirect_response=True, binary_body=False, binary_response=False, options=options))

    async def apple_start(self, query: T.AppleStartQuery | None = None, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """GET /auth/apple/start."""
        return cast("T.HttpRedirect", await self.request('GET', f'/auth/apple/start', query=query, redirect_response=True, binary_body=False, binary_response=False, options=options))

    async def pending_sign_in(self, *, options: RequestOptions | None = None) -> T.PendingSignInDto:
        """GET /auth/connect."""
        return cast("T.PendingSignInDto", await self.request('GET', f'/auth/connect', binary_body=False, binary_response=False, options=options))

    async def pending_sign_in_cancel(self, *, options: RequestOptions | None = None) -> None:
        """POST /auth/connect/cancel."""
        return cast("None", await self.request('POST', f'/auth/connect/cancel', binary_body=False, binary_response=False, options=options))

    async def pending_sign_in_separate(self, *, options: RequestOptions | None = None) -> T.SignInResponse:
        """POST /auth/connect/separate."""
        return cast("T.SignInResponse", await self.request('POST', f'/auth/connect/separate', binary_body=False, binary_response=False, options=options))

    async def email_confirm(self, body: T.EmailConfirmRequest, *, options: RequestOptions | None = None) -> T.SignInResponse:
        """POST /auth/email/confirm."""
        return cast("T.SignInResponse", await self.request('POST', f'/auth/email/confirm', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def email_forgot(self, body: T.EmailAccountRequest, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /auth/email/forgot-password."""
        return cast("T.JsonValue", await self.request('POST', f'/auth/email/forgot-password', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def email_link(self, body: T.EmailLinkRequest, *, options: RequestOptions | None = None) -> T.EmailLinkResponse:
        """POST /auth/email/link."""
        return cast("T.EmailLinkResponse", await self.request('POST', f'/auth/email/link', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def email_login(self, body: T.EmailLoginRequest, *, options: RequestOptions | None = None) -> T.SignInResponse:
        """POST /auth/email/login."""
        return cast("T.SignInResponse", await self.request('POST', f'/auth/email/login', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def email_reset(self, body: T.EmailPasswordRequest, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /auth/email/reset-password."""
        return cast("T.JsonValue", await self.request('POST', f'/auth/email/reset-password', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def email_start(self, body: T.EmailAccountRequest, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /auth/email/start."""
        return cast("T.JsonValue", await self.request('POST', f'/auth/email/start', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def google_callback(self, query: T.GoogleCallbackQuery, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """GET /auth/google/callback."""
        return cast("T.HttpRedirect", await self.request('GET', f'/auth/google/callback', query=query, redirect_response=True, binary_body=False, binary_response=False, options=options))

    async def google_start(self, query: T.GoogleStartQuery | None = None, *, options: RequestOptions | None = None) -> T.HttpRedirect:
        """GET /auth/google/start."""
        return cast("T.HttpRedirect", await self.request('GET', f'/auth/google/start', query=query, redirect_response=True, binary_body=False, binary_response=False, options=options))

    async def logout(self, *, options: RequestOptions | None = None) -> None:
        """POST /auth/logout."""
        return cast("None", await self.request('POST', f'/auth/logout', binary_body=False, binary_response=False, options=options))

    async def logout_all(self, *, options: RequestOptions | None = None) -> None:
        """POST /auth/logout-all."""
        return cast("None", await self.request('POST', f'/auth/logout-all', binary_body=False, binary_response=False, options=options))

    async def mfa_challenge(self, *, options: RequestOptions | None = None) -> T.MfaChallengeDto:
        """GET /auth/mfa."""
        return cast("T.MfaChallengeDto", await self.request('GET', f'/auth/mfa', binary_body=False, binary_response=False, options=options))

    async def mfa_cancel(self, *, options: RequestOptions | None = None) -> None:
        """POST /auth/mfa/cancel."""
        return cast("None", await self.request('POST', f'/auth/mfa/cancel', binary_body=False, binary_response=False, options=options))

    async def mfa_verify(self, body: T.MfaVerifyRequest, *, options: RequestOptions | None = None) -> T.SignInResponse:
        """POST /auth/mfa/verify."""
        return cast("T.SignInResponse", await self.request('POST', f'/auth/mfa/verify', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def auth_options(self, *, options: RequestOptions | None = None) -> T.JsonValue:
        """GET /auth/options."""
        return cast("T.JsonValue", await self.request('GET', f'/auth/options', binary_body=False, binary_response=False, options=options))

    async def device_page(self, query: T.DevicePageQuery | None = None, *, options: RequestOptions | None = None) -> str:
        """GET /device."""
        return cast("str", await self.request('GET', f'/device', query=query, text_response=True, binary_body=False, binary_response=False, options=options))

    async def healthz(self, *, options: RequestOptions | None = None) -> T.HealthResponse:
        """GET /healthz."""
        return cast("T.HealthResponse", await self.request('GET', f'/healthz', binary_body=False, binary_response=False, options=options))

    async def release_install_ps1(self, *, options: RequestOptions | None = None) -> str:
        """GET /install.ps1."""
        return cast("str", await self.request('GET', f'/install.ps1', text_response=True, binary_body=False, binary_response=False, options=options))

    async def release_install_sh(self, *, options: RequestOptions | None = None) -> str:
        """GET /install.sh."""
        return cast("str", await self.request('GET', f'/install.sh', text_response=True, binary_body=False, binary_response=False, options=options))

    async def internal_dead_jobs(self, *, options: RequestOptions | None = None) -> T.Items_DeadJobDto:
        """GET /internal/ops/jobs/dead."""
        return cast("T.Items_DeadJobDto", await self.request('GET', f'/internal/ops/jobs/dead', binary_body=False, binary_response=False, options=options))

    async def internal_retry_job(self, id: str, *, options: RequestOptions | None = None) -> T.DeadJobDto:
        """POST /internal/ops/jobs/{id}/retry."""
        return cast("T.DeadJobDto", await self.request('POST', f'/internal/ops/jobs/{self.segment(id)}/retry', binary_body=False, binary_response=False, options=options))

    async def internal_ledger_check(self, query: T.InternalLedgerCheckQuery, *, options: RequestOptions | None = None) -> T.LedgerCheckResponse:
        """GET /internal/ops/ledger-check."""
        return cast("T.LedgerCheckResponse", await self.request('GET', f'/internal/ops/ledger-check', query=query, binary_body=False, binary_response=False, options=options))

    async def internal_review_version(self, id: str, body: T.ReviewVersionRequest, *, options: RequestOptions | None = None) -> T.ReviewVersionResponse:
        """POST /internal/ops/marketplace/versions/{id}/review."""
        return cast("T.ReviewVersionResponse", await self.request('POST', f'/internal/ops/marketplace/versions/{self.segment(id)}/review', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def internal_ai_output_reports(self, query: T.InternalAiOutputReportsQuery | None = None, *, options: RequestOptions | None = None) -> T.AiOutputReportsOpsResponse:
        """GET /internal/ops/reports/ai-output."""
        return cast("T.AiOutputReportsOpsResponse", await self.request('GET', f'/internal/ops/reports/ai-output', query=query, binary_body=False, binary_response=False, options=options))

    async def internal_resolve_ai_output_report(self, id: str, body: T.ResolveAiOutputReportRequest, *, options: RequestOptions | None = None) -> T.AiOutputReportOpsDto:
        """POST /internal/ops/reports/ai-output/{id}/resolve."""
        return cast("T.AiOutputReportOpsDto", await self.request('POST', f'/internal/ops/reports/ai-output/{self.segment(id)}/resolve', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def internal_usage_summary(self, query: T.InternalUsageSummaryQuery | None = None, *, options: RequestOptions | None = None) -> T.UsageOpsResponse:
        """GET /internal/ops/telemetry/usage."""
        return cast("T.UsageOpsResponse", await self.request('GET', f'/internal/ops/telemetry/usage', query=query, binary_body=False, binary_response=False, options=options))

    async def internal_mfa_reset(self, id: str, *, options: RequestOptions | None = None) -> None:
        """POST /internal/ops/users/{id}/mfa/reset."""
        return cast("None", await self.request('POST', f'/internal/ops/users/{self.segment(id)}/mfa/reset', binary_body=False, binary_response=False, options=options))

    async def publish_release(self, body: T.PublishReleaseRequest, *, options: RequestOptions | None = None) -> T.ReleasePublication:
        """POST /internal/releases."""
        return cast("T.ReleasePublication", await self.request('POST', f'/internal/releases', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def upload_release_artifact(self, body: bytes, query: T.UploadReleaseArtifactQuery, *, options: RequestOptions | None = None) -> T.ReleaseUpload:
        """POST /internal/releases/artifacts."""
        return cast("T.ReleaseUpload", await self.request('POST', f'/internal/releases/artifacts', body=body, query=query, content_type='application/octet-stream', binary_body=True, binary_response=False, options=options))

    async def withdraw_release(self, id: str, *, options: RequestOptions | None = None) -> None:
        """DELETE /internal/releases/{id}."""
        return cast("None", await self.request('DELETE', f'/internal/releases/{self.segment(id)}', binary_body=False, binary_response=False, options=options))

    async def promote_release(self, id: str, *, options: RequestOptions | None = None) -> T.ReleasePublication:
        """POST /internal/releases/{id}/promote."""
        return cast("T.ReleasePublication", await self.request('POST', f'/internal/releases/{self.segment(id)}/promote', binary_body=False, binary_response=False, options=options))

    async def internal_drain_bucket(self, body: T.DrainBucketRequest, *, options: RequestOptions | None = None) -> T.DrainBucketResponse:
        """POST /internal/test/drain-bucket."""
        return cast("T.DrainBucketResponse", await self.request('POST', f'/internal/test/drain-bucket', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def internal_enqueue_job(self, body: T.EnqueueJobRequest, *, options: RequestOptions | None = None) -> T.EnqueueJobResponse:
        """POST /internal/test/enqueue-job."""
        return cast("T.EnqueueJobResponse", await self.request('POST', f'/internal/test/enqueue-job', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def internal_grant(self, body: T.GrantTestRequest, *, options: RequestOptions | None = None) -> T.GrantTestResponse:
        """POST /internal/test/grant."""
        return cast("T.GrantTestResponse", await self.request('POST', f'/internal/test/grant', body=body, content_type='application/json', binary_body=False, binary_response=False, options=options))

    async def oauth_authorize_get(self, query: T.OauthAuthorizeGetQuery | None = None, *, options: RequestOptions | None = None) -> Union[str, T.HttpRedirect]:
        """GET /oauth/authorize."""
        return cast("Union[str, T.HttpRedirect]", await self.request('GET', f'/oauth/authorize', query=query, text_response=True, redirect_response=True, binary_body=False, binary_response=False, options=options))

    async def oauth_authorize_post(self, body: T.ConsentDecisionRequest, *, options: RequestOptions | None = None) -> Union[T.ConsentDecisionResponse, T.HttpRedirect]:
        """POST /oauth/authorize."""
        return cast("Union[T.ConsentDecisionResponse, T.HttpRedirect]", await self.request('POST', f'/oauth/authorize', body=body, content_type='application/json', redirect_response=True, binary_body=False, binary_response=False, options=options))

    async def oauth_device_authorization(self, body: T.OauthDeviceAuthorizationBody, *, options: RequestOptions | None = None) -> T.DeviceAuthorizationResponse:
        """POST /oauth/device_authorization."""
        return cast("T.DeviceAuthorizationResponse", await self.request('POST', f'/oauth/device_authorization', body=body, content_type='application/x-www-form-urlencoded', binary_body=False, binary_response=False, options=options))

    async def oauth_revoke(self, body: T.OauthRevokeBody, *, options: RequestOptions | None = None) -> T.JsonValue:
        """POST /oauth/revoke."""
        return cast("T.JsonValue", await self.request('POST', f'/oauth/revoke', body=body, content_type='application/x-www-form-urlencoded', binary_body=False, binary_response=False, options=options))

    async def oauth_token(self, body: T.OauthTokenBody, *, options: RequestOptions | None = None) -> T.TokenResponse:
        """POST /oauth/token."""
        return cast("T.TokenResponse", await self.request('POST', f'/oauth/token', body=body, content_type='application/x-www-form-urlencoded', binary_body=False, binary_response=False, options=options))

    async def openapi_document(self, *, options: RequestOptions | None = None) -> T.JsonValue:
        """GET /openapi.json."""
        return cast("T.JsonValue", await self.request('GET', f'/openapi.json', binary_body=False, binary_response=False, options=options))

    async def readyz(self, *, options: RequestOptions | None = None) -> T.ReadyResponse:
        """GET /readyz."""
        return cast("T.ReadyResponse", await self.request('GET', f'/readyz', binary_body=False, binary_response=False, options=options))

    async def stripe_webhook(self, body: T.StripeWebhookBody, *, options: RequestOptions | None = None) -> T.StripeWebhookAck:
        """POST /webhooks/stripe."""
        return cast("T.StripeWebhookAck", await self.request('POST', f'/webhooks/stripe', body=body, content_type='text/plain', binary_body=False, binary_response=False, options=options))
